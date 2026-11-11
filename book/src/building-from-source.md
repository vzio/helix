## Building from source

- [Configuring Vortex's runtime files](#configuring-vortexs-runtime-files)
  - [Linux and macOS](#linux-and-macos)
  - [Windows](#windows)
  - [Multiple runtime directories](#multiple-runtime-directories)
  - [Note to packagers](#note-to-packagers)
- [Validating the installation](#validating-the-installation)
- [Configure the desktop shortcut](#configure-the-desktop-shortcut)
- [Building the Debian package](#building-the-debian-package)

Requirements:

Clone the Vortex GitHub repository into a directory of your choice. The
examples in this documentation assume installation into either `~/src/` on
Linux and macOS, or `%userprofile%\src\` on Windows.

- The [Rust toolchain](https://www.rust-lang.org/tools/install)
- The [Git version control system](https://git-scm.com/)
- A C++14 compatible compiler to build the tree-sitter grammars, for example GCC or Clang

If you are using the `musl-libc` standard library instead of `glibc` the following environment variable must be set during the build to ensure tree-sitter grammars can be loaded correctly:

```sh
RUSTFLAGS="-C target-feature=-crt-static"
```

1. Clone the repository:

   ```sh
   git clone https://github.com/vzio/vortex
   cd vortex
   ```

2. Compile from source:

   ```sh
   # Reproducible
   cargo install --path vortex-term --locked
   ```
   ```sh
   # Optimized
   cargo install \
      --profile opt \
      --config 'build.rustflags=["-C", "target-cpu=native"]' \
      --path vortex-term \
      --locked
   ```

   Either command will create the `vx` executable and construct the tree-sitter
   grammars in the local `runtime` folder.

> 💡 If you do not want to fetch or build grammars, set an environment variable `VORTEX_DISABLE_AUTO_GRAMMAR_BUILD`

> 💡 Tree-sitter grammars can be fetched and compiled if not pre-packaged. Fetch
> grammars with `vx --grammar fetch` and compile them with
> `vx --grammar build`. This will install them in
> the `runtime` directory within the user's Vortex config directory (`~/.config/vortex`) (more
> [details below](#multiple-runtime-directories)).

> 💡 If you only want to build _some_ grammars, see [`use-grammars`](./languages.md#choosing-grammars)

### Configuring Vortex's runtime files

#### Linux and macOS

The **runtime** directory is one below the Vortex source, so either export a
`VORTEX_RUNTIME` environment variable to point to that directory and add it to
your `~/.bashrc` or equivalent:

```sh
export VORTEX_RUNTIME=~/src/vortex/runtime
```

Or, create a symbolic link:

```sh
ln -Tsf $PWD/runtime ~/.config/vortex/runtime
```

#### Windows

Either set the `VORTEX_RUNTIME` environment variable to point to the runtime files using the Windows setting (search for
`Edit environment variables for your account`) or use the `setx` command in
Cmd:

```sh
setx VORTEX_RUNTIME "%userprofile%\src\vortex\runtime"
```

> 💡 `%userprofile%` resolves to your user directory like
> `C:\Users\Your-Name\` for example.

Or, create a symlink in `%appdata%\vortex\` that links to the source code directory:

| Method     | Command                                                                                |
| ---------- | -------------------------------------------------------------------------------------- |
| PowerShell | `New-Item -ItemType Junction -Target "runtime" -Path "$Env:AppData\vortex\runtime"`     |
| Cmd        | `cd %appdata%\vortex` <br/> `mklink /D runtime "%userprofile%\src\vortex\runtime"`       |

> 💡 On Windows, creating a symbolic link may require running PowerShell or
> Cmd as an administrator.

#### Multiple runtime directories

When Vortex finds multiple runtime directories it will search through them for files in the
following order:

1. `runtime/` sibling directory to `$CARGO_MANIFEST_DIR` directory (this is intended for
  developing and testing vortex only).
2. `runtime/` subdirectory of OS-dependent vortex user config directory.
3. `$VORTEX_RUNTIME`
4. Distribution-specific fallback directory (set at compile time—not run time—
   with the `VORTEX_DEFAULT_RUNTIME` environment variable)
5. `runtime/` subdirectory of path to Vortex executable.

This order also sets the priority for selecting which file will be used if multiple runtime
directories have files with the same name.

#### Note to packagers

If you are making a package of Vortex for end users, to provide a good out of
the box experience, you should set the `VORTEX_DEFAULT_RUNTIME` environment
variable at build time (before invoking `cargo build`) to a directory which
will store the final runtime files after installation. For example, say you want
to package the runtime into `/usr/lib/vortex/runtime`. The rough steps a build
script could follow are:

1. `export VORTEX_DEFAULT_RUNTIME=/usr/lib/vortex/runtime`
1. `cargo build --profile opt --locked`
1. `cp -r runtime $BUILD_DIR/usr/lib/vortex/`
1. `cp target/opt/vx $BUILD_DIR/usr/bin/vx`

This way the resulting `vx` binary will always look for its runtime directory in
`/usr/lib/vortex/runtime` if the user has no custom runtime in `~/.config/vortex`
or `VORTEX_RUNTIME`.

### Validating the installation

To make sure everything is set up as expected you should run the Vortex health
check:

```sh
vx --health
```

For more information on the health check results refer to
[Health check](https://github.com/helix-editor/helix/wiki/Healthcheck) (an upstream Helix wiki page; the Vortex health check is `vx --health`).

### Configure the desktop shortcut

If your desktop environment supports the
[XDG desktop menu](https://specifications.freedesktop.org/menu-spec/menu-spec-latest.html)
you can configure Vortex to show up in the application menu by copying the
provided `.desktop` file to its correct folder:

```sh
cp contrib/Vortex.desktop ~/.local/share/applications
```

It is recommended to convert the links in the `.desktop` file to absolute paths to avoid potential problems:

```sh
sed -i -e "s|Exec=vx %F|Exec=$(readlink -f ~/.cargo/bin/vx) %F|g" ~/.local/share/applications/Vortex.desktop
```

To use another terminal than the system default, you can modify the `.desktop`
file. For example, to use `kitty`:

```sh
sed -i "s|Exec=vx %F|Exec=kitty vx %F|g" ~/.local/share/applications/Vortex.desktop
sed -i "s|Terminal=true|Terminal=false|g" ~/.local/share/applications/Vortex.desktop
```

### Building the Debian package

Vortex does not publish `.deb` files, but you can build one from source for
your Debian, Ubuntu, or Mint system.

Install `cargo-deb`, the tool used for building the `.deb` file:

```sh
cargo install cargo-deb
```

After cloning and entering the Vortex repository as previously described,
use the following command to build the release binary and package it into a `.deb` file in a single step.

```sh
cargo deb -- --locked
```

> 💡 This locks you into the `--release` profile. But you can also build vortex in any way you like.
> As long as you leave a `target/release/vx` file, it will get packaged with `cargo deb --no-build`

> 💡 Don't worry about the following:
> ```
> warning: Failed to find dependency specification
> ```
> Cargo deb just reports which packaged files it didn't derive dependencies for. But
> so far the dependency deriving seams very good, even if some of the grammar files are skipped.

You can find the resulted `.deb` in `target/debian/`. It should contain everything it needs, including the

- completions for bash, fish, zsh
- .desktop file
- launcher to the binary with the runtime
