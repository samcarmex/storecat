# Storecat user guide

Storecat keeps a catalogue of every drive you own so you can find a file before you go
looking for the drive it is on. This guide covers everyday use. For building from source
see the [README](https://github.com/samcarmex/storecat#readme).

## 1. Catalogue your drives

Open **Drives**. Connected drives that Storecat hasn't seen yet are listed at the top.
Click **Catalogue** and Storecat reads every file's name, path, size and dates. A million
files take about half a minute on an SSD. Nothing on the drive is changed.

- **Folders and network shares.** Use **Add folder or share…** for a NAS share
  (`\\nas\media`), a mapped drive (`Z:\`) or any folder. Shares that are offline simply
  show as not connected; Storecat never waits on them.
- **Discs.** CDs, DVDs and Blu-rays appear like any other drive while the disc is in.
- **After the first scan** Storecat *enriches* the catalogue in the background: quick
  hashes, photo and audio metadata, thumbnails, document text and the contents of zip,
  7z, tar and ISO files. It stops when you unplug the drive and resumes next time.
- **Plug it back in later** and the drive is rescanned silently. Its catalogue is also
  copied to `.storecat\catalog.db` on the drive itself, so another computer can import it.

Give each drive a **role** from the menu on its card: Working, Media, Archive, Backup or
System. Roles are how Storecat decides where suggested moves should go.

The card also shows what the drive is: SSD, NVMe or spinning disk, how it is connected
(USB, SATA, NVMe, network) and the health Windows reports for it. A drive with a health
warning gets an urgent suggestion to copy it to a Backup drive.

## 2. Search

Type in **Search**. Results come from every drive, connected or not, as you type.

| Filter | Example | Meaning |
| --- | --- | --- |
| `ext:` | `ext:pdf` | File extension |
| `kind:` | `kind:video` | video, image, audio, document, archive, code, disk_image, font, design |
| `size:` | `size:>1gb`, `size:10mb..200mb` | File size |
| `modified:` | `modified:2023`, `modified:2024-03`, `after:7d` | Modification date |
| `drive:` | `drive:seagate` | Drive name |
| `path:` | `path:clients` | Folder path contains |
| `is:` | `is:dir`, `is:archived` | Folders, or files inside archives |
| `content:` | `content:"unpaid invoice"` | Text inside documents |
| `has:` | `has:thumb`, `has:text` | Enrichment present |
| `tag:` | `tag:client-x` | Your tags |

Press **Enter** to open a file, or reveal it in Explorer, when its drive is connected.
When it isn't, the detail pane tells you which drive to fetch and where you said it lives.

Save a search with **★ Save** and it appears as a chip on the empty search screen.

**Quick search from anywhere.** In Settings, turn on tray mode and the global shortcut.
Then **Ctrl+Shift+Space** opens a search bar over whatever you are doing.

## 3. See where the space goes

**Storage** shows free space on fast and spinning storage, reclaimable duplicates, and a
treemap of the selected drive. Each block is a folder, coloured by what fills it most
(video, photos, audio...). Click a block to open it; use the path above to go back up.

## 4. Suggestions

The **Suggestions** list in Storage looks at your catalogue and proposes:

| Suggestion | When |
| --- | --- |
| Rescue copy | A drive reports a S.M.A.R.T. warning or failure |
| Drive nearly full | Over 90% used (adjustable) |
| Desktop + Downloads sweep | They hold over 1 GB and the last sweep was over a quarter ago |
| Cold folders | Folders over 1 GB on an SSD untouched for a year, to an Archive drive |
| Duplicates | Over 1 GB wasted by identical copies |
| Catalogued twice | A folder sits inside another catalogued folder or drive |
| Stale catalogue | A drive not seen for six months |
| Roles | No drive has the Archive or Backup role yet |

Suggestions **never run on their own**. **Review move** opens the job in Operations with
its plan: how many files, how much space, what lands where. You start it, or not.
**Later** hides a suggestion for 30 days; **Dismiss** hides it for good. Thresholds and
**Bring back dismissed suggestions** are in Settings.

## 5. Copy and move safely

**Operations** runs copy and move jobs:

1. Pick sources (folders or files), a destination drive and an optional subfolder.
2. **Preview** shows the plan, conflicts and whether there is enough space.
3. **Copy and verify** or **Move and verify** starts it.

Every file is copied through a BLAKE3 hasher into a temporary file, renamed into place,
then read back and compared. A move deletes the original only after that check. The job
is journaled file by file, so pausing, closing Storecat, a crash or unplugging a drive
loses at most the file in flight; **Resume** picks up from there.

**Duplicates** lets you verify a group (full hash of every copy), choose the copy to keep,
and send the rest to the recycle bin.

## 6. Privacy and your data

Everything stays on your computer. Storecat makes exactly two kinds of network request,
both optional: the FFmpeg download for video thumbnails (when you click it) and the daily
update check against GitHub Releases (switch it off in Settings).

Your catalogues live in `%LOCALAPPDATA%\Storecat` on Windows,
`~/Library/Application Support/Storecat` on macOS and `~/.local/share/Storecat` on Linux.
Set `STORECAT_DATA_DIR` to keep them somewhere else, for example on a USB stick.

Logs and crash reports are written to the `logs` folder there and never sent anywhere.
**Settings › Report a problem** opens a GitHub issue with only the version and platform
filled in; attach a log yourself if you want to.

## 7. Command line

The `storecat` command uses the same catalogue (close the app first; they share a lock):

```text
storecat volumes                    list catalogued and connected drives
storecat scan E:\                   catalogue or rescan a drive
storecat search "invoice ext:pdf"   search everything
storecat tree 3 Videos              folder sizes on drive #3
storecat suggest                    the suggestions list
storecat sweep E:\ --dry-run        plan the Desktop + Downloads sweep
storecat backup E:\Backup C:\Work   copy and verify
storecat dupes --verify             duplicate groups
```

## 8. Troubleshooting

- **A drive shows twice.** It was catalogued once as a drive and once as a folder, or a
  folder inside it was added separately. Remove one from its card menu.
- **Scans are slow on Windows.** Antivirus scanning every file Storecat lists can halve
  speed. Excluding the Storecat data folder from real-time scanning helps.
- **Health shows nothing.** USB enclosures often hide S.M.A.R.T. data from Windows.
  Internal SATA and NVMe drives report it.
- **Disc images list only a README.** Some DVDs and Windows installer ISOs use UDF only;
  Storecat reads the ISO 9660 and Joliet directory, which is a stub on those discs.
