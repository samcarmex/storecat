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
