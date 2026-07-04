const std = @import("std");
const TerminalBuildOptions =
    @import("ghostty_src/terminal/build_options.zig").Options;

pub fn build(b: *std.Build) void {
    const optimize = b.standardOptimizeOption(.{});
    const target = b.standardTargetOptions(.{});

    // Ghostty 1.3 unicode pipeline (mirrors vendor/ghostty/src/build/
    // SharedDeps.zig + UnicodeTables.zig): uucode generates its own
    // `tables.zig` from ghostty's build config, then the props/symbols
    // generators (props_uucode.zig / symbols_uucode.zig) run at build time
    // and their stdout becomes the `unicode_tables` / `symbols_tables`
    // anonymous imports.
    const uucode_config = b.path("ghostty_src/build/uucode_config.zig");
    const uucode_tables = blk: {
        const uucode = b.dependency("uucode", .{
            .build_config_path = uucode_config,
        });
        break :blk uucode.namedLazyPath("tables.zig");
    };

    const uucode_host = b.dependency("uucode", .{
        .target = b.graph.host,
        .tables_path = uucode_tables,
        .build_config_path = uucode_config,
    });
    const uucode_target = b.dependency("uucode", .{
        .target = target,
        .tables_path = uucode_tables,
        .build_config_path = uucode_config,
    });

    const props_exe = b.addExecutable(.{
        .name = "props-unigen",
        .root_module = b.createModule(.{
            .root_source_file = b.path("ghostty_src/unicode/props_uucode.zig"),
            .target = b.graph.host,
            .optimize = optimize,
        }),
        // Matches upstream UnicodeTables.zig: x86_64 self-hosted crashes.
        .use_llvm = true,
    });
    props_exe.root_module.addImport("uucode", uucode_host.module("uucode"));

    const symbols_exe = b.addExecutable(.{
        .name = "symbols-unigen",
        .root_module = b.createModule(.{
            .root_source_file = b.path("ghostty_src/unicode/symbols_uucode.zig"),
            .target = b.graph.host,
            .optimize = optimize,
        }),
        .use_llvm = true,
    });
    symbols_exe.root_module.addImport("uucode", uucode_host.module("uucode"));

    const props_run = b.addRunArtifact(props_exe);
    const symbols_run = b.addRunArtifact(symbols_exe);

    // Generated Zig files have to end with .zig (upstream UnicodeTables.zig).
    const wf = b.addWriteFiles();
    const props_output = wf.addCopyFile(props_run.captureStdOut(), "props.zig");
    const symbols_output = wf.addCopyFile(symbols_run.captureStdOut(), "symbols.zig");

    const lib = b.addLibrary(.{
        .name = "ghostty_vt",
        .root_module = b.createModule(.{
            .root_source_file = b.path("lib.zig"),
            .target = target,
            .optimize = optimize,
        }),
        .linkage = .static,
    });
    lib.linkLibC();
    lib.root_module.addImport("uucode", uucode_target.module("uucode"));

    // Ghostty 1.3 terminal sources read `terminal_options` (mirrors
    // GhosttyZig.initVt's lib configuration): plain libghostty-vt shape,
    // no Oniguruma (drops Kitty graphics + tmux control mode), no SIMD
    // (avoids the highway C++ dependency), C ABI off — we export our own.
    const vt_options: TerminalBuildOptions = .{
        .artifact = .lib,
        .oniguruma = false,
        .simd = false,
        .slow_runtime_safety = false,
        .c_abi = false,
    };
    vt_options.add(b, lib.root_module);

    props_output.addStepDependencies(&lib.step);
    lib.root_module.addAnonymousImport("unicode_tables", .{
        .root_source_file = props_output,
    });
    symbols_output.addStepDependencies(&lib.step);
    lib.root_module.addAnonymousImport("symbols_tables", .{
        .root_source_file = symbols_output,
    });

    const include_step = b.addInstallHeaderFile(
        b.path("../include/ghostty_vt.h"),
        "ghostty_vt.h",
    );

    const lib_install = b.addInstallLibFile(lib.getEmittedBin(), "libghostty_vt.a");
    b.getInstallStep().dependOn(&include_step.step);
    b.getInstallStep().dependOn(&lib_install.step);
}
