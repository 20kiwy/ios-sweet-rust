# launchd spawn map — the real exec universe (M3.5 evidence)

Source: `/System/Library/xpc/launchd.plist` from the real iPhone X 16.7.16
OS volume (sha256 810070302b5c4d7c5f00bdc8df20aaea2dd33cd8342d2402fcdf7ff6397d374c, 1,456,980 bytes), cross-referenced with the real `/usr/libexec` inventory
and the `mkiosroot` mini-profile rules (`crates/sweet-core/src/rootfs.rs`).
No guesses: every count below is a join over Apple's own files.

* daemons declared: **532**
* not Disabled (launchd will try to spawn at bootstrap): **496**
* Disabled (spawned only on demand): **36**

## Where the binaries live (spawn fate at bootstrap)

| binary location | daemons | on OS volume | fate in the current mini root |
| --- | --- | --- | --- |
| `/usr/libexec` | 232 | 232 | ENOENT — excluded (highmem budget) |
| `/System/Library/PrivateFrameworks` | 184 | 184 | ENOENT — excluded |
| `/System/Library(other)` | 34 | 34 | ENOENT — excluded |
| `/usr/sbin` | 26 | 23 | exec-able (in root) |
| `/System/Library/CoreServices` | 21 | 21 | ENOENT — excluded |
| `/System/Cryptexes/App/usr/libexec` | 7 | 0 | ENOENT — excluded |
| `OTHER` | 7 | 0 | ENOENT — excluded |
| `/usr/bin` | 6 | 6 | exec-able (in root) |
| `/Developer/usr/local/libexec` | 1 | 0 | ENOENT — excluded |
| `/Applications/ClockAngel.app` | 1 | 1 | ENOENT — excluded |
| `/Applications/InputUI.app` | 1 | 1 | ENOENT — excluded |
| `/Applications/MediaRemoteUI.app` | 1 | 1 | ENOENT — excluded |
| `/Applications/PaperBoard.app` | 1 | 1 | ENOENT — excluded |
| `/Applications/PassbookUISceneService.app` | 1 | 1 | ENOENT — excluded |
| `/Applications/PosterBoard.app` | 1 | 1 | ENOENT — excluded |
| `/Applications/ShortcutsUI.app` | 1 | 1 | ENOENT — excluded |
| `/Applications/ReplayKitAngel.app` | 1 | 1 | ENOENT — excluded |
| `/Applications/AirDropUI.app` | 1 | 1 | ENOENT — excluded |
| `/System/Cryptexes/App/System/Library/CoreServices/SafariSupport.bundle` | 1 | 0 | ENOENT — excluded |
| `/Applications/CheckerBoard.app` | 1 | 1 | ENOENT — excluded |
| `mini-libexec(usermanagerd)` | 1 | 1 | exec-able (explicit add) |
| `System/Library/CoreServices/ScreenSharingServer.app` | 1 | 1 | ENOENT — excluded |
| `/System/Cryptexes/OS/System/Library/Frameworks/WebKit.framework/Daemons` | 1 | 0 | ENOENT — excluded |

### Binaries NOT on the OS volume (cryptex-resident or absent)

* `/Developer/usr/local/libexec/NFQRCoded` — absent from this build
* `/System/Cryptexes/App/System/Library/CoreServices/SafariSupport.bundle/SafariBookmarksSyncAgent` — App/OS cryptex member
* `/System/Cryptexes/App/usr/libexec/AuthenticationServicesAgent` — App/OS cryptex member
* `/System/Cryptexes/App/usr/libexec/com.apple.Safari.History` — App/OS cryptex member
* `/System/Cryptexes/App/usr/libexec/passwordbreachd` — App/OS cryptex member
* `/System/Cryptexes/App/usr/libexec/safarifetcherd` — App/OS cryptex member
* `/System/Cryptexes/App/usr/libexec/webbookmarksd` — App/OS cryptex member
* `/System/Cryptexes/App/usr/libexec/webinspectord` — App/OS cryptex member
* `/System/Cryptexes/App/usr/libexec/webpushd` — App/OS cryptex member
* `/System/Cryptexes/OS/System/Library/Frameworks/WebKit.framework/Daemons/adattributiond` — App/OS cryptex member
* `/usr/sbin/dietapplecamerad` — absent from this build
* `/usr/sbin/dietappleh13camerad` — absent from this build
* `/usr/sbin/sntpd` — absent from this build
* `OTACrashCopier` — relative/malformed path
* `aslmanager` — relative/malformed path
* `assetsd` — relative/malformed path
* `cloudphotod` — relative/malformed path
* `com.apple.photos.ImageConversionService` — relative/malformed path
* `com.apple.photos.VideoConversionService` — relative/malformed path
* `ftp-proxy` — relative/malformed path

## md0 byte cost of the excluded groups (real file sizes)

| group | daemons measured | bytes (MiB) |
| --- | --- | --- |
| `/usr/libexec` | 232 | 196.7 |
| `/System/Library/PrivateFrameworks` | 184 | 110.4 |
| `/System/Library(other)` | 34 | 66.2 |
| `/usr/sbin` | 23 | 46.8 |
| `/System/Library/CoreServices` | 21 | 7.4 |
| `/usr/bin` | 6 | 1.8 |
| `mini-libexec(usermanagerd)` | 1 | 0.7 |
| `/Applications/CheckerBoard.app` | 1 | 0.4 |
| `System/Library/CoreServices/ScreenSharingServer.app` | 1 | 0.4 |
| `/Applications/ClockAngel.app` | 1 | 0.3 |
| `/Applications/AirDropUI.app` | 1 | 0.3 |
| `/Applications/ReplayKitAngel.app` | 1 | 0.3 |
| `/Applications/ShortcutsUI.app` | 1 | 0.2 |
| `/Applications/MediaRemoteUI.app` | 1 | 0.2 |
| `/Applications/InputUI.app` | 1 | 0.1 |
| `/Applications/PosterBoard.app` | 1 | 0.1 |
| `/Applications/PassbookUISceneService.app` | 1 | 0.1 |
| `/Applications/PaperBoard.app` | 1 | 0.1 |

(sizes cover only the daemons' own binaries; frameworks come from the DSC)

## Users launchd must resolve (getpwnam via usermanagerd muser)

* `mobile` — 373 daemons
* `root` — 132 daemons
* `_wireless` — 5 daemons
* `_diskimagesiod` — 2 daemons
* `_securityd` — 2 daemons
* `_networkd` — 2 daemons
* `_datadetectors` — 1 daemons
* `_reportmemoryexception` — 1 daemons
* `_accessoryupdater` — 1 daemons
* `_analyticsd` — 1 daemons
* `_captiveagent` — 1 daemons
* `nobody` — 1 daemons
* `_distnote` — 1 daemons
* `_ftp` — 1 daemons
* `_gpsd` — 1 daemons
* `_iconservices` — 1 daemons
* `_logd` — 1 daemons
* `_mdnsresponder` — 1 daemons
* `_installd` — 1 daemons
* `_sntpd` — 1 daemons
* `_timed` — 1 daemons
* `_trustd` — 1 daemons

## M5/M6 Mach-service gate owners

* `com.apple.backboard.display.services`: `com.apple.backboardd`
* `com.apple.iohideventsystem`: `com.apple.backboardd`

## First wave to add when the md0 budget allows (ordered by boot criticality)

1. `/usr/libexec` selective adds — the smallest set whose MachServices the
   next userland stage actually requests, NOT the whole directory
   (whole-dir cost is in the table above).
2. Trim candidates to pay for it: `/usr/share` (docs/locales),
   `/usr/local` (empty on iOS), `/usr/lib` non-boot tails — each trim is
   measured against the 0xfcc00000 bank before it ships (mkadt constraint).

## Reproduce (all real-IPSW, no local copies)

```sh
sweet-rust ipsw-file  "$IPSW_URL" /System/Library/xpc/launchd.plist --out launchd.plist
sweet-rust ipsw-ls    "$IPSW_URL" /usr/libexec > usr-libexec.txt
# one absolute path per line (the 524 unique ProgramArguments[0] values):
sweet-rust ipsw-check-paths "$IPSW_URL" --paths daemon-binaries.txt > daemon-existence.txt
```

`ipsw-check-paths` resolves every path against a single volume open
(catalog walk once, then per-path B-tree lookups) — the batch form of
`ipsw-stat`. The spawn-map generator joins these three outputs with the
`mkiosroot` mini-profile rules.
