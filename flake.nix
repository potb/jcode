{
  description = "Fast, reproducible jcode development shells";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nixpkgs, rust-overlay, ... }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
          };
          inherit (pkgs) lib;

          # The lock file pins the overlay revision, so "latest" remains stable
          # until someone intentionally updates flake.lock.
          selectedNightly = pkgs.rust-bin.selectLatestNightlyWith (
            toolchain:
            toolchain.minimal.override {
              extensions = [
                "clippy"
                "rust-src"
                "rustfmt"
              ];
            }
          );
          nightlyComponents = selectedNightly.availableComponents;

          # Use one identical rustc/cargo path in every shell. Adding clippy and
          # rustfmt through a second aggregate would change the compiler's Nix
          # store path and make Cargo rebuild cached dependencies when moving
          # between the full and selfdev shells.
          nightlyToolchain = selectedNightly.override { extensions = [ ]; };

          # Libraries used by the TUI binary. Keeping these in the minimal
          # selfdev shell makes its persistent Nix profile a GC root for every
          # Nix-store path that a published selfdev binary needs at runtime.
          tuiRuntimeLibraries = (
            with pkgs;
            [
              oniguruma
              openssl
              zlib
            ]
          );

          desktopRuntimeLibraries = lib.optionals pkgs.stdenv.isLinux (
            with pkgs;
            [
              fontconfig
              freetype
              libGL
              libx11
              libxcb
              libxcursor
              libxi
              libxkbcommon
              libxrandr
              vulkan-loader
              wayland
            ]
          );

          # Keep the automatically entered shells intentionally small. These are
          # the tools needed by dev_cargo.sh and the workspace's native builds.
          buildTools = with pkgs; [
            bashInteractive
            cacert
            cmake
            git
            gnumake
            mold
            openssh
            patchelf
            perl
            pkg-config
            procps
            util-linux
          ];

          # Editor, lint, benchmark, and fallback tools belong in explicit human
          # development shells, not in every daemon-triggered selfdev build.
          developerTools =
            (with pkgs; [
              cargo-nextest
              cargo-watch
              clang
              curl
              hyperfine
              jq
              lld
              ninja
              nixfmt
              python3
              rust-analyzer
              sccache
              shellcheck
              shfmt
            ])
            ++ [
              nightlyComponents.clippy
              nightlyComponents.rustfmt
            ];

          mkJcodeShell =
            {
              shellName,
              extraTools ? [ ],
              extraLibraries ? [ ],
              includeRustSource ? false,
            }:
            let
              runtimeLibraries = tuiRuntimeLibraries ++ extraLibraries;
            in
            pkgs.mkShell (
              {
                name = "jcode-${shellName}";

                nativeBuildInputs = [ nightlyToolchain ] ++ buildTools ++ extraTools;
                buildInputs = runtimeLibraries;

                # Keep Cargo's mutable caches outside the Nix store. Cargo's
                # default CARGO_HOME (~/.cargo) and this checkout's target/
                # therefore survive every nix develop invocation.
                CARGO_REGISTRIES_CRATES_IO_PROTOCOL = "sparse";
                JCODE_NIX_DEVSHELL_ACTIVE = "1";
                JCODE_NIX_DEVSHELL_NAME = shellName;
                JCODE_NIX_TOOLCHAIN_CHANNEL = "nightly";

                shellHook = ''
                  export LD_LIBRARY_PATH="${lib.makeLibraryPath runtimeLibraries}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

                  if [[ "''${JCODE_NIX_QUIET:-0}" != "1" ]]; then
                    printf 'jcode dev shell: %s, Cargo cache %s, target cache %s\n' \
                      "$JCODE_NIX_DEVSHELL_NAME" \
                      "''${CARGO_HOME:-$HOME/.cargo}" \
                    "$PWD/target"
                  fi
                '';
              }
              // lib.optionalAttrs includeRustSource {
                RUST_SRC_PATH = "${nightlyComponents."rust-src"}/lib/rustlib/src/rust/library";
              }
            );

          selfdevShell = mkJcodeShell {
            shellName = "selfdev";
          };
          desktopShell = mkJcodeShell {
            shellName = "desktop";
            extraLibraries = desktopRuntimeLibraries;
          };
          fullShell = mkJcodeShell {
            shellName = "full";
            extraTools = developerTools;
            extraLibraries = desktopRuntimeLibraries;
            includeRustSource = true;
          };
        in
        {
          # Fast human/direnv default. Opt into `full` for editor/lint tools.
          default = selfdevShell;
          full = fullShell;

          # Daemon-triggered builds select one of these smaller profiles.
          selfdev = selfdevShell;
          desktop = desktopShell;
        }
      );
    };
}
