# Installing Vortex

Vortex is not packaged anywhere: there are no pre-built binaries and no
package manager provides it. Vortex is installed by
[building it from source](./building-from-source.md), which creates the `vx`
executable.

Note that:

- The packages in your system's package manager install upstream
  [Helix](https://github.com/helix-editor/helix), not Vortex.
  See [Package managers](./package-managers.md).

- To take full advantage of Vortex, install the language servers for your
  preferred programming languages. The upstream Helix
  [wiki](https://github.com/helix-editor/helix/wiki/Language-Server-Configurations)
  has instructions that apply to Vortex too.

After building, run `vx --health` to see which runtime directories `vx`
searches and which languages and language servers are available. If
necessary, you can override the default runtime location by setting the
`VORTEX_RUNTIME` environment variable (see
[Configuring Vortex's runtime files](./building-from-source.md#configuring-vortexs-runtime-files)).
