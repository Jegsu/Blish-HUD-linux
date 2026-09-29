# Blish HUD bridge

A reimplementation of [SorryQuick's external-dx11-overlay](https://github.com/SorryQuick/external-dx11-overlay).
See [Credits](#credits) for what comes from it.

Renders Blish HUD inside Guild Wars 2, as part of the game's own frames. That makes it work
under Wine and Proton — on any compositor, in fullscreen too — where an overlay window on top
of the game does not.

Blish HUD runs as its own hidden process and renders into a pair of shared textures. The bridge
is a dll that runs inside the game and connects the two: it draws the latest of those textures
over each frame, forwards the game's mouse input to Blish, and keeps clicks on Blish's windows
from reaching the game.

### What is different

Mouse clicks and the scroll wheel are now sent to Blish over UDP, the same way cursor movement
already was, instead of being read by Blish HUD's global `WH_MOUSE_LL` hook. Clicks on Blish's
windows are kept from the game by the bridge itself.

That hook sat in the path of every mouse event on the system. Under Wine, the delay it added
while the game was turning the camera threw off the game's cursor tracking, and the cursor
would jump to a corner of the screen. With the hook gone, that no longer happens.

## Building

Everything builds in Docker containers, so the only thing you need installed is Docker with
the Compose plugin. Rust, MinGW and .NET are all provided by the containers.

1. **Clone the repository with its submodules.** Blish HUD needs the one in `thirdparty/`:

   ```sh
   git clone --recurse-submodules https://github.com/Jegsu/Blish-HUD-linux.git
   cd Blish-HUD-linux
   ```

   If you already cloned without submodules, run `git submodule update --init --recursive`.

2. **Build.** From the repository root:

   ```sh
   ./build.sh
   ```

   The first run takes a while, as it downloads the build images and dependencies. Later runs
   are quick.

3. **Find the output:**

   | What | Where |
   |---|---|
   | The bridge | `bridge/target/x86_64-pc-windows-gnu/release/d3d11.dll` |
   | Blish HUD | `Blish HUD/bin/x64/Release/net472/` |

The Blish HUD build does not include its `Content/` folder: that needs a font only available on
Windows. Installing covers where to get it.

To build the bridge without Docker, install Rust (the toolchain in `rust-toolchain.toml` is
picked up automatically) and MinGW-w64, then run `cargo build --release` in `bridge/`. Blish HUD
itself can also be built on Windows with Visual Studio; see the main README.

## Installing

When you're done, the game folder looks like this:

```
Guild Wars 2/
├── Gw2-64.exe
├── d3d11.dll                  ← the bridge
└── addons/
    └── blishhud/              ← Blish HUD
        ├── Blish HUD.exe
        ├── Blish HUD.exe.config
        ├── ref.dat
        ├── Content/
        └── de/  es/  fr/
```

1. **Copy the bridge.** Put the built `d3d11.dll` next to `Gw2-64.exe`.

2. **Copy Blish HUD.** Put everything from the built `Blish HUD/bin/x64/Release/net472/` into
   `addons/blishhud/`.

3. **Add Blish's `Content/` folder.** The build can't produce it, so copy it from a Blish HUD
   release of the same version into `addons/blishhud/`. Without it, Blish closes right after
   starting.

4. **Start the game** from Steam, as usual. There is no launcher or injector: Blish HUD starts
   with the game and closes when the game does.

### Good to know

- **Blish's settings and modules** are stored in the game's Wine prefix, under
  `Documents/Guild Wars 2/addons/blishhud/`.
- **Plain Wine without Proton:** if the bridge doesn't load, set
  `WINEDLLOVERRIDES="d3d11=n,b"`. Proton needs nothing extra.
- **Something not showing up?** Check `addons/blishhud-bridge/logs/`. It logs each step, so the
  last line says how far it got.

### With arcdps

arcdps is also installed as `d3d11.dll`, so the two need to share:

1. Rename arcdps's `d3d11.dll` to `arcdps.dll`.
2. Put the bridge's `d3d11.dll` in its place.
3. In `addons/blishhud-bridge/bridge.ini`, set `chainload = arcdps.dll` under `[general]`.
   The file is created the first time you start the game with the bridge installed.

The bridge then loads arcdps and passes the game's D3D11 calls through it, so arcdps works as
it always does.

Alternatively, install arcdps as `dxgi.dll` and leave `chainload` empty; the two then load
separately. Don't do both, or arcdps loads twice.

## Settings

The first run writes `addons/blishhud-bridge/bridge.ini` with the defaults below. Settings go
in its `[general]` section; paths are relative to the game directory.

| Setting | Default | |
|---|---|---|
| `launch_blish` | `true` | Start Blish HUD with the game |
| `blish_path` | `addons/blishhud/Blish HUD.exe` | Blish HUD's executable |
| `chainload` | empty (disabled) | A d3d11 proxy to route through, such as `arcdps.dll` |

Keybinds live in a `[keybinds]` section; writing one replaces the defaults.

| Default | Action | |
|---|---|---|
| `Ctrl+Alt+P` | `dump_state` | Log the bridge's state and rebuild its renderer |
| `Ctrl+Alt+O` | `restart_blish` | Restart the Blish HUD the bridge started |
| `Ctrl+Alt+B` | `toggle_rendering` | Stop or resume drawing Blish HUD |

Keys are `A`–`Z`, `0`–`9` and `F1`–`F24`, with any of `Ctrl`, `Alt` and `Shift`.

Logs are written to `addons/blishhud-bridge/logs/` and kept for a day. They also go to
stdout, which Wine shows in the terminal the game was started from.

## Development

Before sending changes, run the checks from the repository root:

```sh
./check.sh   # rustfmt, clippy with warnings as errors, and the unit tests
```

### Layout

```
src/
├── lib.rs        crate docs, lint policy, DllMain
├── error.rs      the Error type
├── protocol.rs   ┐ platform-independent, unit tested:
├── clicks.rs     │ the wire format shared with Blish's C# side,
├── keybind.rs    │ click routing, key combinations,
├── config.rs     ┘ and bridge.ini
└── runtime/      Windows only — runs inside the game
    ├── mod.rs        startup sequence and keybind actions
    ├── proxy.rs      the d3d11 exports and the chain to arcdps / system d3d11
    ├── hook.rs       the Present detour
    ├── renderer.rs   drawing Blish's frame
    ├── link.rs       shared memory and kernel objects shared with Blish
    ├── input.rs      the subclassed window procedure
    ├── launcher.rs   starting Blish HUD, and stopping it with the game
    └── window.rs, paths.rs, logging.rs, sys.rs
```

`protocol.rs` is the contract with `ExternalDirectxOverlay.cs`; change both together.

Two rules hold throughout, because this code runs inside someone else's process:

- **Nothing may take the game down.** Errors are values and get logged; every entry point the
  game calls catches panics, which is why the release profile unwinds. `unwrap`, `expect` and
  `panic!` are linted outside tests, and `./check.sh` fails on them.
- **Every `unsafe` block states why it is sound**, in a `// SAFETY:` comment. Clippy checks
  this too.

## Credits

This is based on [external-dx11-overlay](https://github.com/SorryQuick/external-dx11-overlay)
by [SorryQuick](https://github.com/SorryQuick), together with the matching
[Blish HUD fork](https://github.com/SorryQuick/Blish-HUD) and
[Gw2-Simple-Addon-Loader](https://github.com/SorryQuick/Gw2-Simple-Addon-Loader). The approach
is his: hiding Blish HUD's window and drawing its frames inside the game from shared D3D11
textures, the shared-memory header and kernel objects the two sides coordinate through, and
sending the game's mouse input to Blish over UDP. That is what makes Blish HUD usable on Linux
at all.

The two precompiled shaders in `shaders/` are his, copied unmodified.

## License

Apache License 2.0, as for the original external-dx11-overlay; see [LICENSE](LICENSE). The rest
of this repository is Blish HUD, under the MIT license.
