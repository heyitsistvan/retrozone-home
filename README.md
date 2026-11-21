# RetroZone Home

Play the games on your computer from your other devices, through [RetroZone](https://retrozone.app).

RetroZone keeps your library - what each game is, your saves, replays, scores and achievements -
but never the games themselves: those stay on your own disk. A browser on that computer reads
them from the folder you let it into. A browser anywhere else has nothing to read them from,
unless RetroZone Home runs on the computer the games are on.

It sits in the tray and lets RetroZone know it is there. When one of your devices wants a game
it does not have, RetroZone introduces the two, and they connect directly (WebRTC): the game
goes from your computer to that device, and nowhere else. No game passes through RetroZone, and
the device keeps no copy.

## Using it

Open it, and:

1. **Sign in with RetroZone.** Your browser opens a RetroZone page that names this computer and
   shows a code; the app shows the same code. Allow it there.
2. **Choose a folder** of games - with the folder picker, or by dropping one on the window. As
   many as you like; each can be shown in Explorer or removed from the list.
3. That is all. The first time, it reads every game once to note its checksums, several files at
   a time (40 GB took 12 seconds on a fast SSD with the files already cached; a slower disk takes
   longer). A device that asks for a game meanwhile is told to try again shortly. After that only
   new or changed files are read.

Closing the window leaves it running in the tray, by the clock - it says so the first time. The
tray icon opens the window again, opens RetroZone, or quits. *Start with Windows* (off until you
switch it on) starts it in the tray when you sign in to Windows. *Sign out* has RetroZone forget
this computer.

It signs in to `https://retrozone.app` unless told otherwise: *Change*, under the sign-in button,
takes another address (`RETROZONE_SITE` at build time changes the default, `config::DEFAULT_SITE`).

Settings, the list of games and the log are kept in `%APPDATA%\RetroZone Home` (`config.json`,
`index.json`, `home.log`). The key in `config.json` lets in whoever has it as this computer.

## What it sends, and to whom

- Only a file in its own index, found by its checksum - never a path somebody names.
- Only to a device that came through RetroZone under the key this computer was signed in with,
  which RetroZone hands over only for your own signed-in devices.
- At most four devices at once.

The window's page can only call the app's own commands (`capabilities/default.json`); choosing
folders, opening the browser and starting with Windows are done by the app itself.

## How it talks to RetroZone

Everything goes to the address it signed in to, as JSON, over HTTPS.

| Request | What |
| --- | --- |
| `POST retro/computer/start` | Starts a sign-in: the code to show, the page to open, and a secret to ask with |
| `POST retro/computer/collect` | How the sign-in went; once the player allowed it, the key (handed over once) |
| `POST retro/computer/asks` | Whether a device wants this computer. Held open until one does, or about twenty seconds, then asked again; the answer also says which file extensions a game can have and which STUN servers to use |
| `POST retro/computer/calls/{id}/answer` | The answer to a device's WebRTC offer, or none when four are connected |
| `POST retro/computer/leave` | Signs out |

After sign-in every request carries the key in the `X-RetroZone-Key` header. Once connected, the
device asks for each file on a WebRTC data channel of its own, named after it -
`{"want":"game","sha1":"…"}` for the file a game is in, `{"want":"part","crc":"…"}` for one file
out of an arcade set - and gets a line of JSON (`{"size":…,"name":"…"}` or `{"error":"…"}`)
followed by the bytes (`src/peer.rs`).

A game is found by the SHA-1 RetroZone knows it by: for a zipped cartridge the file inside the
zip, otherwise the whole file. Every file is noted by both where they can differ, and every file
inside a zip by its CRC-32 (`src/library.rs`). Zips are read the way RetroZone's own browser code
reads them - their list, then each file where the list says it is (`src/zipped.rs`) - so a zip a
stricter reader turns down still counts.

## Building it

```
cargo build --release      # target/release/retrozone-home.exe
```

That is the app, ready to run. It is Rust, with [Tauri](https://tauri.app) 2 for the window;
WebView2, which draws it, comes with Windows 10 and 11. An installer is configured
(`tauri.conf.json`, NSIS) but needs the Tauri CLI to build (`cargo tauri build`).

| File | What |
| --- | --- |
| `src/main.rs` | The window and tray: the commands the page calls, closing to the tray, starting with Windows |
| `ui/` | The window's page, in RetroZone's look (Fraunces by Undercase Type, SIL Open Font License: `ui/fonts/OFL-Fraunces.txt`) |
| `src/engine.rs` | Signing in and out, following the folders, and being there for the player's devices |
| `src/site.rs` | Talking to RetroZone |
| `src/peer.rs` | A device connected to this computer, and sending it what it asks for |
| `src/library.rs` | The folders, and what is in them by checksum |
| `src/zipped.rs` | Reading zips |
| `src/config.rs` | What the app remembers |
| `tauri.conf.json`, `capabilities/`, `icons/` | Tauri's: the window, what its page may do, the icon |

For trying it out:

- `RETROZONE_HOME_DIR` keeps settings and the list somewhere else - a second copy, or a test one;
- `RETROZONE_HOME_NO_BROWSER=1` notes the sign-in page in the log instead of opening the browser;
- `--hidden` starts it in the tray without the window;
- `RUST_LOG=retrozone_home=debug` says more in the log; `RUST_LOG=debug` includes the WebRTC library's own;
- `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9333` opens a debugging port a
  puppeteer script can drive the window through;
- `retrozone-home debug-index <folder> <extensions> <out>` writes every SHA-1 a folder's files are
  known by.

## Not done yet

A relay (TURN) for networks that will not let two devices connect directly; a signed installer;
updates.

Known: with the window hidden in the tray, a program that closes another by sending its "main
window" WM_CLOSE (as .NET's `CloseMainWindow()` does) hits the single-instance plugin's helper
window instead, and a second start then exits without bringing the window back. The X button
cannot do it - the window is shown then - and the tray icon still opens it.
