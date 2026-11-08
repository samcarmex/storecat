//! `storecat` command line: the same engine as the desktop app, scriptable.
//!
//! ```text
//! storecat volumes                     list detected and catalogued volumes
//! storecat add <identity|path>         catalogue a detected volume (by identity) or a folder
//! storecat scan <id|path> [--no-enrich] (re)scan a volume, then enrich it
//! storecat enrich <id> [--hdd|--ssd]   hashes, metadata, thumbnails, text, archives
//! storecat search <query...>           search; --limit N --sort relevance|modified|size
//! storecat dupes [--all] [--verify]    duplicate groups across all drives
//! storecat export <id> <dir>           copy a volume's catalogue out
//! storecat import <file>               bring a catalogue back in
//! storecat ffmpeg [--download]         show or install the FFmpeg sidecar
//! storecat remove <id>                 forget a volume and its catalogue
//! storecat version
//! ```

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use anyhow::{anyhow, bail, Context};
use storecat_core::{
    enrich::DiskMode, Catalog, DataDirs, DupQuery, DupScope, DupSort, EnrichOptions, EnrichPhase,
    EnrichProgress, ScanOptions, ScanPhase, ScanProgress, SearchOptions, SortBy, Volume,
};

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("--help");
    let rest = &args[args.len().min(1)..];
    match cmd {
        "--help" | "-h" | "help" => print_help(),
        "--version" | "-V" | "version" => {
            let info = storecat_core::build_info();
            println!(
                "storecat {} (core {}, {} build, {})",
                env!("CARGO_PKG_VERSION"),
                info.core_version,
                info.profile,
                storecat_platform::name()
            );
        }
        "volumes" | "ls" => cmd_volumes()?,
        "add" => cmd_add(rest)?,
        "scan" => cmd_scan(rest)?,
        "enrich" => cmd_enrich(rest)?,
        "search" | "s" => cmd_search(rest)?,
        "dupes" | "duplicates" => cmd_dupes(rest)?,
        "export" => cmd_export(rest)?,
        "import" => cmd_import(rest)?,
        "ffmpeg" => cmd_ffmpeg(rest)?,
        "remove" | "rm" => cmd_remove(rest)?,
        "sweep" => cmd_sweep(rest)?,
        "backup" | "copy" | "move" => {
            let mut a: Vec<String> = rest.to_vec();
            if cmd == "move" {
                a.push("--move".into());
            }
            cmd_backup(&a)?
        }
        "jobs" | "job" => cmd_jobs(rest)?,
        "tags" | "tag" => cmd_tags(rest)?,
        "saved" => cmd_saved(rest)?,
        "tree" | "du" => cmd_tree(rest)?,
        "suggest" | "suggestions" => cmd_suggest(rest)?,
        other => {
            eprintln!("unknown command: {other}\n");
            print_help();
            std::process::exit(2);
        }
    }
    Ok(())
}

fn open() -> anyhow::Result<std::sync::Arc<Catalog>> {
    let dirs = DataDirs::resolve();
    let cat = Catalog::open(dirs).context("opening catalogue")?;
    cat.refresh_detected(storecat_platform::detect_volumes())?;
    cat.refresh_folders(&storecat_platform::is_remote_path)?;
    if cat.index_rebuilding() {
        eprintln!("Search index is being rebuilt for this version; this happens once …");
        let started = Instant::now();
        cat.wait_for_index();
        eprintln!("Index rebuilt in {:.1}s.", started.elapsed().as_secs_f64());
    }
    Ok(cat)
}

fn cmd_volumes() -> anyhow::Result<()> {
    let cat = open()?;
    let vols = cat.volumes()?;
    println!("Catalogued volumes ({}):", vols.len());
    if vols.is_empty() {
        println!("  (none)  — add one with `storecat add <identity|path>`");
    }
    for v in &vols {
        print_volume(&cat, v);
    }
    let avail = cat.detected_uncatalogued()?;
    println!("\nConnected, not catalogued ({}):", avail.len());
    for d in &avail {
        println!(
            "  {:<8} {:<24} {:<6} {:>9}  {:<4} {}",
            d.mount_path,
            truncate(&d.label, 24),
            d.fs,
            human_bytes(d.capacity),
            d.media_type.as_str(),
            d.identity
        );
    }
    Ok(())
}

fn print_volume(cat: &Catalog, v: &Volume) {
    let status = if v.online {
        format!("online at {}", v.mount_path.clone().unwrap_or_default())
    } else {
        match v.last_seen_at {
            Some(t) => format!("offline, last seen {}", fmt_time(t)),
            None => "offline".to_string(),
        }
    };
    let scanned = match v.last_scan_at {
        Some(t) => format!(
            "{} files, {} in {:.1}s on {}",
            v.file_count,
            human_bytes(v.total_bytes),
            v.scan_seconds,
            fmt_time(t)
        ),
        None => "never scanned".to_string(),
    };
    let cov = cat.enrich_coverage(v.id).unwrap_or_default();
    let mut profile = vec![v.role.as_str().to_string()];
    if !v.bus.is_empty() {
        profile.push(v.bus.clone());
    }
    if !v.model.is_empty() {
        profile.push(v.model.clone());
    }
    if v.health != storecat_core::Health::Unknown {
        profile.push(format!("health {}", v.health.as_str()));
    }
    println!(
        "  #{:<3} {:<24} {:<5} {:>9}  {}\n       {}\n       {}\n       enriched {}/{} · {} thumbs · {} texts · {} hashed",
        v.id,
        truncate(&v.label, 24),
        v.media_type.as_str(),
        human_bytes(v.capacity),
        status,
        profile.join(" · "),
        scanned,
        cov.done,
        cov.files,
        cov.thumbs,
        cov.texts,
        cov.hashed
    );
}

fn cmd_add(args: &[String]) -> anyhow::Result<()> {
    let target = args
        .first()
        .ok_or_else(|| anyhow!("usage: storecat add <identity|path>"))?;
    let cat = open()?;
    let v = add_target(&cat, target)?;
    println!("Added volume #{} \"{}\"", v.id, v.label);
    Ok(())
}

fn add_target(cat: &Catalog, target: &str) -> anyhow::Result<Volume> {
    if Path::new(target).is_dir() {
        if let Some(d) = cat
            .detected()
            .into_iter()
            .find(|d| same_mount(&d.mount_path, target))
        {
            return Ok(cat.add_detected(&d.identity)?);
        }
        return Ok(cat.add_folder(Path::new(target), None)?);
    }
    Ok(cat.add_detected(target)?)
}

fn same_mount(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.trim_end_matches(['\\', '/']).to_lowercase();
    norm(a) == norm(b)
}

fn cmd_scan(args: &[String]) -> anyhow::Result<()> {
    let target = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .ok_or_else(|| anyhow!("usage: storecat scan <id|path> [--no-enrich]"))?;
    let no_enrich = args.iter().any(|a| a == "--no-enrich");
    let cat = open()?;
    let volume = match target.parse::<i64>() {
        Ok(id) => cat.volume(id)?,
        Err(_) => add_target(&cat, target)?,
    };
    println!("Scanning #{} \"{}\" …", volume.id, volume.label);
    let started = Instant::now();
    let last_len = AtomicUsize::new(0);
    let progress = |p: ScanProgress| {
        let mut out = std::io::stderr().lock();
        match &p.phase {
            ScanPhase::Walking => {
                let line = format!(
                    "\r  {:>9} files  {:>6} dirs  {:>9}  {:>5.1}s  {}",
                    p.files,
                    p.dirs,
                    human_bytes(p.bytes),
                    p.elapsed_ms as f64 / 1000.0,
                    truncate(&p.current, 50)
                );
                let pad = last_len.load(Ordering::Relaxed).saturating_sub(line.len());
                let _ = write!(out, "{line}{}", " ".repeat(pad));
                last_len.store(line.len(), Ordering::Relaxed);
            }
            ScanPhase::Committing => {
                let _ = write!(out, "\r  committing index …{}", " ".repeat(60));
            }
            ScanPhase::Done | ScanPhase::Failed { .. } => {
                let _ = writeln!(out);
            }
        }
        let _ = out.flush();
    };
    let stats = cat.scan(volume.id, &ScanOptions::default(), &progress)?;
    let secs = started.elapsed().as_secs_f64();
    println!(
        "Done: {} files, {} dirs, {} in {:.1}s ({:.0} files/s). {} unreadable, {} symlinks skipped.",
        stats.files,
        stats.dirs,
        human_bytes(stats.bytes),
        secs,
        stats.files as f64 / secs.max(0.001),
        stats.errors,
        stats.skipped_symlinks
    );
    if !no_enrich {
        run_enrich(&cat, volume.id, EnrichOptions::default())?;
    }
    Ok(())
}

fn cmd_enrich(args: &[String]) -> anyhow::Result<()> {
    let id: i64 = args
        .first()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("usage: storecat enrich <id> [--hdd|--ssd] [--all-folders]"))?;
    let disk = if args.iter().any(|a| a == "--hdd") {
        DiskMode::Spinning
    } else if args.iter().any(|a| a == "--ssd") {
        DiskMode::Solid
    } else {
        DiskMode::Auto
    };
    let opts = EnrichOptions {
        disk,
        skip_bulk_folders: !args.iter().any(|a| a == "--all-folders"),
        ..Default::default()
    };
    let cat = open()?;
    run_enrich(&cat, id, opts)
}

fn run_enrich(cat: &Catalog, id: i64, opts: EnrichOptions) -> anyhow::Result<()> {
    let v = cat.volume(id)?;
    let pending = cat
        .enrich_coverage(id)
        .map(|c| c.files.saturating_sub(c.done))
        .unwrap_or(0);
    if pending == 0 {
        println!("Nothing to enrich for #{} \"{}\".", v.id, v.label);
        return Ok(());
    }
    let ff = cat.ffmpeg_status();
    println!(
        "Enriching #{} \"{}\": {} files pending{} …",
        v.id,
        v.label,
        pending,
        if ff.available {
            ""
        } else {
            " (no FFmpeg: video thumbnails skipped)"
        }
    );
    let last_len = AtomicUsize::new(0);
    let progress = |p: EnrichProgress| {
        let mut out = std::io::stderr().lock();
        match &p.phase {
            EnrichPhase::Running => {
                let line = format!(
                    "\r  {} {:>8}/{:<8} {:>5} thumbs {:>5} texts {:>5} meta {:>6} hashed {:>5.0}s  {}",
                    match p.pass {
                        0 => "planning",
                        1 => "quick   ",
                        _ => "details ",
                    },
                    p.done,
                    p.total,
                    p.thumbs,
                    p.texts,
                    p.metas,
                    p.hashed,
                    p.elapsed_ms as f64 / 1000.0,
                    truncate(&p.current, 36)
                );
                let pad = last_len.load(Ordering::Relaxed).saturating_sub(line.len());
                let _ = write!(out, "{line}{}", " ".repeat(pad));
                last_len.store(line.len(), Ordering::Relaxed);
            }
            _ => {
                let _ = writeln!(out);
            }
        }
        let _ = out.flush();
    };
    let s = cat.enrich(id, &opts, &progress)?;
    println!(
        "{}: {} files in {:.1}s — {} hashed, {} metadata, {} thumbnails, {} texts, {} archives, {} errors.",
        if s.paused { "Paused" } else { "Enriched" },
        s.processed,
        s.elapsed_ms as f64 / 1000.0,
        s.hashed,
        s.metas,
        s.thumbs,
        s.texts,
        s.archives,
        s.errors
    );
    Ok(())
}

fn cmd_search(args: &[String]) -> anyhow::Result<()> {
    let mut limit = 25usize;
    let mut sort = SortBy::Relevance;
    let mut terms: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--limit" | "-n" => {
                i += 1;
                limit = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| anyhow!("--limit needs a number"))?;
            }
            "--sort" => {
                i += 1;
                sort = match args.get(i).map(String::as_str) {
                    Some("modified") | Some("date") => SortBy::ModifiedDesc,
                    Some("size") => SortBy::SizeDesc,
                    Some("relevance") | None => SortBy::Relevance,
                    Some(o) => bail!("unknown sort {o}"),
                };
            }
            s => terms.push(s.to_string()),
        }
        i += 1;
    }
    let query = terms.join(" ");
    let cat = open()?;
    let r = cat.search(
        &query,
        &SearchOptions {
            limit,
            sort,
            ..Default::default()
        },
    )?;
    for h in &r.hits {
        let status = if h.volume_online { " " } else { "○" };
        let path = if h.path.is_empty() {
            h.name.clone()
        } else {
            format!("{}/{}", h.path, h.name)
        };
        let marks = format!(
            "{}{}{}",
            if h.flags & storecat_core::flags::HAS_THUMB != 0 {
                "▣"
            } else {
                " "
            },
            if h.flags & storecat_core::flags::HAS_TEXT != 0 {
                "T"
            } else {
                " "
            },
            if h.item_type == storecat_core::ItemType::ArchiveEntry {
                "z"
            } else {
                " "
            },
        );
        println!(
            "{status}{marks} {:<9} {:<12} {:<20} {}",
            if h.is_dir {
                "<dir>".to_string()
            } else {
                human_bytes(h.size)
            },
            fmt_time(h.mtime),
            truncate(&h.volume_label, 20),
            path
        );
    }
    println!(
        "{} of {} results in {:.1} ms  (○ offline · ▣ thumbnail · T text · z inside archive)",
        r.hits.len(),
        r.total,
        r.elapsed_ms
    );
    Ok(())
}

fn cmd_dupes(args: &[String]) -> anyhow::Result<()> {
    let verify = args.iter().any(|a| a == "--verify");
    let scope = if args.iter().any(|a| a == "--all") {
        DupScope::All
    } else {
        DupScope::Mine
    };
    let cat = open()?;
    let page = cat.duplicate_page(&DupQuery {
        scope,
        sort: DupSort::Waste,
        desc: true,
        limit: 50,
        offset: 0,
        ..Default::default()
    })?;
    let summary = page.summary;
    println!(
        "{} duplicate groups, {} files, {} reclaimable (files ≥ {}).",
        summary.groups,
        summary.files,
        human_bytes(summary.waste),
        human_bytes(cat.hash_min_bytes())
    );
    let mut groups = page.groups;
    if verify {
        for g in groups.iter_mut() {
            if !g.verified {
                if let Ok(v) = cat.verify_group(&g.hash) {
                    *g = v;
                }
            }
        }
    }
    for g in &groups {
        println!(
            "\n{} × {}  {}  {}{}",
            g.count,
            human_bytes(g.size),
            g.name,
            if g.verified {
                "✓ verified"
            } else {
                "~ quick hash"
            },
            if g.waste > 0 {
                format!("  (free {})", human_bytes(g.waste))
            } else {
                String::new()
            }
        );
        for m in &g.members {
            println!(
                "    {} {:<20} {}/{}{}",
                if m.volume_online { " " } else { "○" },
                truncate(&m.volume_label, 20),
                m.path,
                m.name,
                if m.app_managed { "  (app copy)" } else { "" }
            );
        }
    }
    Ok(())
}

fn cmd_export(args: &[String]) -> anyhow::Result<()> {
    let id: i64 = args
        .first()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("usage: storecat export <id> <dir>"))?;
    let dir = args
        .get(1)
        .ok_or_else(|| anyhow!("usage: storecat export <id> <dir>"))?;
    let cat = open()?;
    let p = cat.export_volume(id, Path::new(dir))?;
    println!("Exported to {}", p.display());
    Ok(())
}

fn cmd_import(args: &[String]) -> anyhow::Result<()> {
    let file = args
        .first()
        .ok_or_else(|| anyhow!("usage: storecat import <file>"))?;
    let cat = open()?;
    let v = cat.import_volume(Path::new(file))?;
    println!("Imported #{} \"{}\": {} files", v.id, v.label, v.file_count);
    Ok(())
}

fn cmd_ffmpeg(args: &[String]) -> anyhow::Result<()> {
    let cat = open()?;
    if args.iter().any(|a| a == "--download") {
        println!("Downloading FFmpeg (about 150 MB) …");
        let last = AtomicUsize::new(0);
        let p = storecat_core::tools::download_ffmpeg(&cat.dirs, &|done, total| {
            let mb = (done / (1024 * 1024)) as usize;
            if mb != last.swap(mb, Ordering::Relaxed) {
                let _ = write!(
                    std::io::stderr(),
                    "\r  {} MB{}",
                    mb,
                    total
                        .map(|t| format!(" / {} MB", t / (1024 * 1024)))
                        .unwrap_or_default()
                );
            }
        })?;
        eprintln!();
        println!("Installed {}", p.ffmpeg.display());
    }
    let s = cat.ffmpeg_status();
    if s.available {
        println!(
            "FFmpeg {} at {}",
            s.version.unwrap_or_else(|| "?".into()),
            s.ffmpeg.unwrap_or_default()
        );
    } else {
        println!("FFmpeg not found. Run `storecat ffmpeg --download` or install it on your PATH.");
    }
    Ok(())
}

fn cmd_remove(args: &[String]) -> anyhow::Result<()> {
    let id: i64 = args
        .first()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("usage: storecat remove <id>"))?;
    let cat = open()?;
    let v = cat.volume(id)?;
    cat.remove_volume(id)?;
    println!("Removed #{} \"{}\"", v.id, v.label);
    Ok(())
}

fn print_help() {
    println!(
        "storecat — offline drive catalogue\n\n\
         USAGE:\n  storecat <command> [args]\n\n\
         COMMANDS:\n  \
         volumes                  List catalogued and connected volumes\n  \
         add <identity|path>      Catalogue a connected volume or a folder\n  \
         scan <id|path>           Scan (or rescan) a volume, then enrich it (--no-enrich to skip)\n  \
         enrich <id> [--hdd|--ssd] [--all-folders]\n                           \
         Hashes, metadata, thumbnails, text and archive listings\n  \
         search <query> [-n N] [--sort relevance|modified|size]\n  \
         dupes [--all] [--verify] Duplicate groups (--all: app folders too)\n  \
         export <id> <dir>        Copy a volume's catalogue out\n  \
         import <file>            Bring a catalogue back in\n  \
         ffmpeg [--download]      Show or install the FFmpeg sidecar (video thumbnails)\n  \
         sweep <dest> [--move] [--dry-run]   Back up Desktop + Downloads into <dest>\\Archive\\<YYYY-MM> ...\n  \
         backup <dest> <src>... [--move]     Copy folders/files to <dest>, verified (--move deletes originals)\n  \
         jobs [run <id>|rm <id>]  List, resume or delete jobs\n  \
         tags [add|rm <tag> <query>]  List tags, or tag every hit of a query\n  \
         saved [add <name> <query>|rm <id>]  Saved searches\n  \
        tree <volume-id> [folder]       Folder sizes, largest first\n  \
        suggest [dismiss <key> [days]|reset]  Storage suggestions\n  \
         remove <id>              Forget a volume\n  \
         version                  Print version information\n\n\
         Data directory: {}  (override with STORECAT_DATA_DIR)",
        DataDirs::resolve().root.display()
    );
}

fn human_bytes(b: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

fn fmt_time(t: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(t)
        .map(|d| format!("{:04}-{:02}-{:02}", d.year(), d.month() as u8, d.day()))
        .unwrap_or_default()
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

// ----- Phase 3: jobs, tags, saved searches ------------------------------------------------

fn cmd_sweep(args: &[String]) -> anyhow::Result<()> {
    let dest = args.iter().find(|a| !a.starts_with("--")).ok_or_else(|| {
        anyhow!("usage: storecat sweep <dest drive or folder> [--move] [--dry-run]")
    })?;
    let kind = if args.iter().any(|a| a == "--move") {
        storecat_core::JobKind::Move
    } else {
        storecat_core::JobKind::Copy
    };
    let cat = open()?;
    let spec = cat.sweep_spec(dest, kind);
    run_spec(&cat, spec, args.iter().any(|a| a == "--dry-run"))
}

fn cmd_backup(args: &[String]) -> anyhow::Result<()> {
    // storecat backup <dest> <src>... [--move] [--dry-run] [--skip|--overwrite]
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    if positional.len() < 2 {
        bail!("usage: storecat backup <dest folder> <source>... [--move] [--dry-run] [--skip|--overwrite]");
    }
    let kind = if args.iter().any(|a| a == "--move") {
        storecat_core::JobKind::Move
    } else {
        storecat_core::JobKind::Copy
    };
    let conflict = if args.iter().any(|a| a == "--skip") {
        storecat_core::ConflictPolicy::Skip
    } else if args.iter().any(|a| a == "--overwrite") {
        storecat_core::ConflictPolicy::Overwrite
    } else {
        storecat_core::ConflictPolicy::Rename
    };
    let cat = open()?;
    let spec = storecat_core::JobSpec {
        kind,
        name: format!(
            "{} to {}",
            if kind == storecat_core::JobKind::Move {
                "Move"
            } else {
                "Back up"
            },
            positional[0]
        ),
        sources: positional[1..].iter().map(|s| s.to_string()).collect(),
        dest_root: positional[0].to_string(),
        conflict,
        verify: true,
        checks: Vec::new(),
    };
    run_spec(&cat, spec, args.iter().any(|a| a == "--dry-run"))
}

fn run_spec(cat: &Catalog, spec: storecat_core::JobSpec, dry_run: bool) -> anyhow::Result<()> {
    let plan = cat.plan_job(&spec)?;
    println!(
        "{}: {} files in {} folders, {} → {}",
        spec.name,
        plan.files,
        plan.folders,
        human_bytes(plan.bytes),
        spec.dest_root
    );
    if let Some(free) = plan.dest_free {
        println!(
            "  destination has {} free{}",
            human_bytes(free),
            if plan.enough_space {
                ""
            } else {
                "  ← NOT ENOUGH"
            }
        );
    }
    if plan.conflicts > 0 {
        println!(
            "  {} file(s) already exist at the destination ({:?})",
            plan.conflicts, spec.conflict
        );
    }
    for m in &plan.missing_sources {
        println!("  missing: {m}");
    }
    for (s, d) in plan.sample.iter().take(5) {
        println!("  {s}\n    → {d}");
    }
    if dry_run {
        println!("Dry run; nothing copied.");
        return Ok(());
    }
    if !plan.enough_space {
        bail!("not enough free space at the destination");
    }
    let job = cat.create_job(&spec)?;
    run_job_cli(cat, job.id)
}

fn run_job_cli(cat: &Catalog, id: i64) -> anyhow::Result<()> {
    let last_len = AtomicUsize::new(0);
    let progress = |p: storecat_core::JobProgress| {
        let mut out = std::io::stderr().lock();
        if p.status == storecat_core::JobStatus::Running {
            let line = format!(
                "\r  {:>6}/{:<6} {:>9}/{:<9} {:>7}/s  {}",
                p.files_done,
                p.files_total,
                human_bytes(p.bytes_done),
                human_bytes(p.bytes_total),
                human_bytes(p.bytes_per_sec),
                truncate(&p.current, 40)
            );
            let pad = last_len.load(Ordering::Relaxed).saturating_sub(line.len());
            let _ = write!(out, "{line}{}", " ".repeat(pad));
            last_len.store(line.len(), Ordering::Relaxed);
        } else {
            let _ = writeln!(out);
        }
        let _ = out.flush();
    };
    let job = cat.run_job(id, &progress)?;
    println!(
        "Job #{} {:?}: {} of {} files, {} failed, {} skipped{}",
        job.id,
        job.status,
        job.files_done,
        job.files_total,
        job.files_failed,
        job.files_skipped,
        job.error.map(|e| format!(" — {e}")).unwrap_or_default()
    );
    if job.files_failed > 0 {
        for it in cat.job_items(job.id, Some(storecat_core::ItemStatus::Failed), 20)? {
            println!("  ✗ {}: {}", it.source, it.error.unwrap_or_default());
        }
    }
    Ok(())
}

fn cmd_jobs(args: &[String]) -> anyhow::Result<()> {
    let cat = open()?;
    match args.first().map(String::as_str) {
        Some("run") | Some("resume") => {
            let id: i64 = args
                .get(1)
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| anyhow!("usage: storecat jobs run <id>"))?;
            run_job_cli(&cat, id)
        }
        Some("rm") | Some("delete") => {
            let id: i64 = args
                .get(1)
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| anyhow!("usage: storecat jobs rm <id>"))?;
            cat.delete_job(id)?;
            println!("Deleted job #{id}");
            Ok(())
        }
        _ => {
            let jobs = cat.jobs()?;
            if jobs.is_empty() {
                println!(
                    "No jobs yet. Try `storecat sweep E:\\` or `storecat backup <dest> <src>...`."
                );
            }
            for j in jobs {
                println!(
                    "#{:<3} {:<7} {:<9} {:>5}/{:<5} {:>9}  {}{}",
                    j.id,
                    j.kind.as_str(),
                    j.status.as_str(),
                    j.files_done,
                    j.files_total,
                    human_bytes(j.bytes_total),
                    j.name,
                    if j.files_failed > 0 {
                        format!("  ({} failed)", j.files_failed)
                    } else {
                        String::new()
                    }
                );
            }
            Ok(())
        }
    }
}

fn cmd_tags(args: &[String]) -> anyhow::Result<()> {
    let cat = open()?;
    if args.is_empty() {
        for t in cat.tags()? {
            println!("{:<30} {}", t.name, t.count);
        }
        return Ok(());
    }
    // storecat tags add|rm <tag> <query>  → tag every hit of the query (first 500)
    let (op, tag, query) = (
        args[0].as_str(),
        args.get(1)
            .ok_or_else(|| anyhow!("usage: storecat tags add|rm <tag> <query>"))?,
        args[2..].join(" "),
    );
    let r = cat.search(
        &query,
        &SearchOptions {
            limit: 500,
            ..Default::default()
        },
    )?;
    let mut n = 0;
    for h in r
        .hits
        .iter()
        .filter(|h| h.item_type != storecat_core::ItemType::ArchiveEntry)
    {
        let res = match op {
            "add" => cat.tag_item(h.volume_id, h.item_id, h.item_type, tag),
            "rm" | "remove" => cat.untag_item(h.volume_id, h.item_id, h.item_type, tag),
            _ => bail!("unknown tag operation {op}"),
        };
        if res.is_ok() {
            n += 1;
        }
    }
    println!("{op} \"{tag}\" on {n} item(s)");
    Ok(())
}

fn cmd_saved(args: &[String]) -> anyhow::Result<()> {
    let cat = open()?;
    match args.first().map(String::as_str) {
        Some("add") => {
            let name = args
                .get(1)
                .ok_or_else(|| anyhow!("usage: storecat saved add <name> <query>"))?;
            let s = cat.save_search(name, &args[2..].join(" "))?;
            println!("Saved #{}: {} = {}", s.id, s.name, s.query);
        }
        Some("rm") => {
            let id: i64 = args
                .get(1)
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| anyhow!("usage: storecat saved rm <id>"))?;
            cat.delete_saved_search(id)?;
            println!("Removed #{id}");
        }
        _ => {
            for s in cat.saved_searches()? {
                println!("#{:<3} {:<24} {}", s.id, s.name, s.query);
            }
        }
    }
    Ok(())
}

fn cmd_tree(args: &[String]) -> anyhow::Result<()> {
    let id: i64 = args
        .first()
        .and_then(|a| a.trim_start_matches('#').parse().ok())
        .ok_or_else(|| anyhow!("usage: storecat tree <volume-id> [folder path inside it]"))?;
    let cat = open()?;
    let started = Instant::now();
    let mut tree = cat.folder_tree(id, None)?;
    if let Some(sub) = args.get(1) {
        for seg in sub.split(['/', '\\']).filter(|s| !s.is_empty()) {
            let next = tree
                .children
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(seg))
                .map(|c| c.id)
                .ok_or_else(|| anyhow!("no folder named {seg} under {}", tree.node.path))?;
            tree = cat.folder_tree(id, Some(next))?;
        }
    }
    let crumbs: Vec<&str> = tree.crumbs.iter().map(|c| c.name.as_str()).collect();
    println!(
        "{}  {} in {} files  ({} ms)",
        crumbs.join(" / "),
        human_bytes(tree.node.bytes),
        tree.node.files,
        started.elapsed().as_millis()
    );
    let total = tree.node.bytes.max(1) as f64;
    for c in tree.children.iter().take(25) {
        let pct = c.bytes as f64 / total * 100.0;
        println!(
            "  {:>9} {:>5.1}%  {:<40} {:>8} files  mostly {:?}, newest {}",
            human_bytes(c.bytes),
            pct,
            truncate(&c.name, 40),
            c.files,
            c.top_kind,
            fmt_time(c.newest)
        );
    }
    for f in tree.files.iter().take(10) {
        println!(
            "  {:>9}         {}",
            human_bytes(f.size),
            truncate(&f.name, 60)
        );
    }
    if tree.rest_count > 0 {
        println!(
            "  {:>9}         … and {} more",
            human_bytes(tree.rest_bytes),
            tree.rest_count
        );
    }
    Ok(())
}

fn cmd_suggest(args: &[String]) -> anyhow::Result<()> {
    use storecat_core::suggest::SuggestionAction;
    let cat = open()?;
    match args.first().map(String::as_str) {
        Some("dismiss") | Some("snooze") => {
            let key = args
                .get(1)
                .ok_or_else(|| anyhow!("usage: storecat suggest dismiss <key> [days]"))?;
            let days = args.get(2).and_then(|d| d.parse().ok());
            cat.dismiss_suggestion(key, days)?;
            println!(
                "{} {key}",
                if days.is_some() {
                    "Snoozed"
                } else {
                    "Dismissed"
                }
            );
            return Ok(());
        }
        Some("reset") => {
            cat.reset_suggestions()?;
            println!("All dismissed and snoozed suggestions are back.");
            return Ok(());
        }
        _ => {}
    }
    let started = Instant::now();
    let list = cat.suggestions()?;
    println!(
        "{} suggestions ({} ms)",
        list.len(),
        started.elapsed().as_millis()
    );
    for s in &list {
        println!("\n  [{:?}] {}", s.severity, s.title);
        println!("    {}", s.body);
        match &s.action {
            Some(SuggestionAction::Job { spec, .. }) => println!(
                "    job: {:?} {} -> {}",
                spec.kind,
                spec.sources.join(", "),
                spec.dest_root
            ),
            Some(SuggestionAction::View { view, .. }) => println!("    open: {view}"),
            None => {}
        }
        println!("    key: {}", s.key);
    }
    Ok(())
}
