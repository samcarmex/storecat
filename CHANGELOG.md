# Changelog

## 0.9.0 (release candidate)

The feature-complete build for 1.0, for testing on real drive collections.

**Storage smarts**
- Drive profiles: SSD, NVMe or spinning disk, connection (USB, SATA, NVMe, network,
  optical), model and the health Windows reports, refreshed whenever a drive connects.
  Catalogued folders inherit their drive's profile.
- Storage view with a treemap per drive, coloured by content type, with drill-down, and a
  content-type breakdown.
- Suggestions inbox: rescue copies for failing drives, nearly-full drives, the Desktop +
  Downloads sweep, cold folders on fast storage, duplicates, folders catalogued twice,
  stale catalogues and missing roles. Each opens as a reviewable job; nothing runs by itself.
  Thresholds are adjustable; suggestions can be snoozed or dismissed.
- Folder sizes are computed once per scan (about a second per million files).

**Sources**
- Network shares and mapped drives are checked off the UI thread with a timeout, so an
  offline NAS never stalls the app; their free space is tracked.
- Optical discs are detected; ISO 9660 / Joliet / Rock Ridge disc images are listed like
  archives, so files inside `.iso` files are searchable.
- Overlapping catalogues (a folder inside another catalogued folder) are flagged and no
  longer double-counted in totals.

**Fixes from testing**
- A malformed PDF could crash the app during enrichment. Files a parser can't read are
  now skipped and noted in the log.
- On Windows, catalogues moved out of the program folder to `%LOCALAPPDATA%\io.storecat.app`,
  where the uninstaller's "Delete the application data" option can find them. Existing
  catalogues move automatically on first start.
- No `.storecat` folder is written to the root of the system drive any more.

**Faster enrichment**
- Enrichment plans its work once per run instead of querying the catalogue for every
  batch of 400 files, and files stream through the workers, so no thread waits on the
  slowest file of a batch.
- Two passes: hashes, details, photo thumbnails and archive contents first (duplicates
  and photo search work early), then document text and video thumbnails.
- Spinning hard drives and optical discs are read one file at a time in the order the
  files sit on the disk, while decoding carries on in parallel. Photos there are
  thumbnailed from the preview the camera embedded, instead of reading the whole photo.
- PDFs are read and parsed once for both their details and their text, and text
  extraction stops at the 1 MB limit instead of parsing every page.
- Package, cache and system folders (`site-packages`, `.venv`, `.cargo`, caches, editor
  extensions, the Android SDK, Deno's cache, coding agents' `.claude/worktrees` copies,
  `Windows`, `Program Files` and similar) keep names and hashes but get no text or
  thumbnails.
- `.env` files are no longer read for text: they hold passwords and keys.
- Small `.mts` files are TypeScript, not video, and small `.bin` files are no longer
  counted as disk images.

**Thumbnails from what files already carry**
- Album art from MP3, FLAC, M4A, Ogg, Opus, WAV and other tagged audio.
- The preview Word, Excel and PowerPoint save in their files, LibreOffice documents'
  thumbnails, e-book covers (EPUB), Photoshop thumbnails, and the previews in Krita,
  Sketch, Procreate, Keynote/Pages/Numbers and 3MF files.
- Drives enriched by an earlier version pick up the artwork they skipped by themselves:
  only those files are read again, and their hashes, details and document text are kept.

**Closing, resuming and progress**
- Closing the window now quits Storecat (unless "keep running in the tray" is on). It
  used to stay running in the background, which blocked updates and stopped it opening
  again. Opening Storecat while it runs brings the window forward.
- Quitting pauses enrichment and copy jobs cleanly; both carry on next time.
- A scan cut off by quitting, a crash, a power cut or unplugging restarts by itself when
  the app opens or the drive comes back. Enrichment carries on from the last file it
  saved, and search catches up on any files saved just before the interruption.
- Drives waiting for another drive's enrichment say "Queued" and which drive they follow.
- Drive cards show enrichment as a progress bar with a percentage and the file being read
  on its own line, and a freshly scanned drive prompts "Enrich" as the next step instead
  of showing "0% enriched".
- The Storage view's content bar ends with free space, and shows space the scan did not
  catalogue (system folders) separately. Its "Disk image" slice is now "Disk images &
  VMs": on a PC that is mostly WSL, Docker and virtual machine disks.
- Scanning shows an estimated percentage (against the last scan, or the space in use on
  a first scan), and the indexing step that follows shows its own bar instead of
  "Writing index…".
- A drive's free space holds still while it is scanned or enriched. The catalogue may
  live on that drive, and its database and index grow and shrink as they are written.
- The one-time search index rebuild after an update shows its progress above the results.
- Starting Storecat straight after quitting it no longer fails while the old instance
  finishes closing.
- Finished jobs show what they did ("1.5 GB freed · 12 files recycled", "4 GB copied")
  instead of a full progress bar. The Copy and Move choice spans the form.
- Sharper installer artwork on high-DPI displays.

**Search**
- Click the Name, Size or Modified heading to sort; click again to reverse. Typing words
  returns to best match first; filter-only searches such as `ext:mp3` list by name.
- Choosing Audio, Video, Images, Documents, Design or Archives shows the extensions
  you actually have as chips, with counts. Tick one or several to narrow the list.
- `ext: mp3`, `ext:.mp3`, `*.mp3`, `.mp3` and `type:mp3` all mean the same as `ext:mp3`.

**Changes show up by themselves**
- While Storecat runs it watches connected drives and catalogued folders. Files added,
  changed, renamed or deleted outside the app appear in search within a few seconds,
  without a rescan. Renamed files and folders keep their hashes, details and thumbnails;
  new and changed files are enriched shortly after.
- Storecat's own data folder and the package, cache and system folders scans skip are
  ignored. Folder sizes are worked out again the next time the Storage view opens.
- When the system loses track of changes (a very large burst of them), the folder
  concerned is compared with the disk again, or the drive rescanned in the background,
  at most once every ten minutes per drive.
- Network shares are not watched: notifications from other computers don't arrive
  reliably. They keep relying on rescans, as do changes made while Storecat was closed.
- Settings → Behaviour → "Pick up changes while Storecat runs" turns it off.

**Duplicates you can act on**
- Columns sort by clicking their heading; the default is largest reclaimable space first.
- Filters: a name or folder search, file type, a drive, and "Ready to clean" for groups
  whose copy to keep and an extra copy are both connected. The totals follow the filters.
- Every group offers the copy to keep (outside Temp and Downloads, not a "(1)" copy) and
  **Recycle other copies**, without verifying first: each copy is compared byte for
  byte with the kept one just before it is recycled, and left alone if it differs. Tick
  groups to clean up many in one job.
- Copies inside app-managed folders (package caches, build output, `AppData`, dot
  folders) are never removed and don't count as reclaimable. The list shows your files by
  default; "Include app folders" shows the rest.

**Leftover files from macOS and Windows**
- `._name`, `.DS_Store`, `Thumbs.db` and `desktop.ini` files are hidden from search
  (`is:leftover` shows them), and so are the same files inside zips and other archives,
  along with the `__MACOSX` folder Mac-made zips carry. The search index rebuilds once
  after updating.
- A suggestion offers a job that recycles a drive's leftovers; `._` files are checked to
  really be macOS metadata first.

**Release**
- Signed auto-update from GitHub Releases (daily check, opt-out, install on click).
- Local log file and crash reports, "Report a problem" with version info only.
- Branded Windows installers (NSIS and MSI), winget manifest, CI builds for Windows,
  macOS and Linux, platform support for macOS and Linux volume detection.

## 0.3.0

Verified copy and move jobs with resume, duplicate cleanup, tags, saved searches, drive
photos, tray mode and the quick-search palette.

## 0.2.0

Enrichment: hashes, metadata, thumbnails, document text, archive contents, duplicates.

## 0.1.0

Cataloguing and instant offline search.
