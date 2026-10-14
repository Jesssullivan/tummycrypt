"""Bazel rule for building the TCFS macOS installer package.

The rule wraps scripts/macos-build-pkg.sh with declared release artifacts. It
does not fetch artifacts, notarize, staple, or discover signing credentials.
Those remain explicit caller-owned inputs and post-build proof gates.
"""

def _tcfs_macos_pkg_impl(ctx):
    output_name = ctx.attr.output_name
    if not output_name:
        output_name = ctx.label.name + ".pkg"

    pkg = ctx.actions.declare_file(output_name)
    args = ctx.actions.args()
    args.add("--version", ctx.attr.version)
    args.add("--cli-tar", ctx.file.cli_tar)
    args.add("--fileprovider-zip", ctx.file.fileprovider_zip)
    args.add("--postinstall", ctx.file.postinstall)
    args.add("--output", pkg)
    args.add("--identifier", ctx.attr.identifier)
    if ctx.attr.signing_identity:
        args.add("--sign", ctx.attr.signing_identity)

    ctx.actions.run(
        executable = ctx.executable.build_script,
        arguments = [args],
        inputs = [
            ctx.file.cli_tar,
            ctx.file.fileprovider_zip,
            ctx.file.postinstall,
        ],
        tools = [
            ctx.executable.build_script,
            ctx.executable.structure_smoke,
        ],
        outputs = [pkg],
        env = {
            "TCFS_PKG_STRUCTURE_SMOKE": ctx.executable.structure_smoke.path,
        },
        mnemonic = "TcfsMacosPkg",
        progress_message = "Building TCFS macOS package %{label}",
        execution_requirements = {
            "requires-darwin-packaging-tools": "1",
        },
    )

    return [DefaultInfo(files = depset([pkg]))]

tcfs_macos_pkg = rule(
    implementation = _tcfs_macos_pkg_impl,
    attrs = {
        "build_script": attr.label(
            default = Label("//scripts:macos-build-pkg.sh"),
            allow_single_file = True,
            cfg = "exec",
            executable = True,
        ),
        "cli_tar": attr.label(
            allow_single_file = True,
            mandatory = True,
        ),
        "fileprovider_zip": attr.label(
            allow_single_file = True,
            mandatory = True,
        ),
        "identifier": attr.string(default = "io.tinyland.tcfs"),
        "output_name": attr.string(),
        "postinstall": attr.label(
            default = Label("//scripts:macos-pkg-postinstall.sh"),
            allow_single_file = True,
        ),
        "signing_identity": attr.string(),
        "structure_smoke": attr.label(
            default = Label("//scripts:macos-pkg-structure-smoke.sh"),
            allow_single_file = True,
            cfg = "exec",
            executable = True,
        ),
        "version": attr.string(mandatory = True),
    },
    doc = "Build a TCFS macOS .pkg from declared CLI and FileProvider release artifacts.",
)
