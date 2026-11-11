## Package managers

Vortex is not packaged: it is not available through apt, dnf, pacman, Nix,
Flatpak, Snap, Homebrew, MacPorts, winget, Scoop, Chocolatey or any other
package manager, and it has no AppImage or pre-built release binaries.

The packages of upstream [Helix](https://github.com/helix-editor/helix) install
upstream Helix, not Vortex, and they know nothing of Vortex's features.

To use Vortex, [build it from source](./building-from-source.md). That gives
you the `vx` executable.
