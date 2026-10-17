{
  description = "tinyclient: Slack, Discord, Telegram, Teams, Signal and Messages in one small native window";

  inputs = {
    nixpkgs.url = "https://flakehub.com/f/NixOS/nixpkgs/0.1";
    fenix = {
      url = "https://flakehub.com/f/nix-community/fenix/0.1";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {self, ...} @ inputs: let
    # No x86_64-darwin: nixpkgs dropped Intel Macs in 26.11.
    supportedSystems = [
      "x86_64-linux"
      "aarch64-linux"
      "aarch64-darwin"
    ];
    forEachSupportedSystem = f:
      inputs.nixpkgs.lib.genAttrs supportedSystems (
        system:
          f {
            pkgs = import inputs.nixpkgs {
              inherit system;
              overlays = [
                inputs.self.overlays.default
              ];
            };
          }
      );
  in {
    overlays.default = final: prev: let
      fenixPkgs = inputs.fenix.packages.${prev.stdenv.hostPlatform.system};
    in {
      # The README and CI build with current stable Rust; there is no rust-toolchain.toml.
      rustToolchain = fenixPkgs.combine (
        with fenixPkgs.stable; [
          clippy
          rustc
          cargo
          rustfmt
          rust-src
        ]
      );
      tinyclient = final.callPackage ./packaging/nix/package.nix {};
    };

    # Linux only: on macOS a call needs the app bundle, which macOS grants the microphone to.
    packages = forEachSupportedSystem (
      {pkgs}:
        inputs.nixpkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
          default = pkgs.tinyclient;
          inherit (pkgs) tinyclient;
        }
    );

    # `programs.tinyclient` for Home Manager: the package, and the chatd user service.
    homeManagerModules.default = import ./packaging/nix/home-manager.nix self;

    devShells = forEachSupportedSystem (
      {pkgs}: let
        inherit (pkgs) lib stdenv;

        # Opened with dlopen, so nothing links them and `cargo run` must find them on
        # LD_LIBRARY_PATH: wgpu's Vulkan loader and its EGL fallback, the Wayland client (GPUI
        # loads it only in a Wayland session), and the X11 side of the window.
        runtimeLibs = with pkgs; [
          vulkan-loader
          libGL
          wayland
          libxkbcommon
          libx11
          libxcb
          fontconfig
          freetype
        ];

        # What CI installs with apt (packaging/ci/apt.sh): native-tls (openssl),
        # cpal (alsa-lib), and the login helper's WebKitGTK sign-in window (gtk3, webkitgtk, libsoup).
        linuxLibs = with pkgs;
          runtimeLibs
          ++ [
            openssl
            alsa-lib
            glib
            gtk3
            webkitgtk_4_1
            libsoup_3
          ];
      in {
        default = pkgs.mkShell {
          packages = with pkgs; [
            rustToolchain
            rust-analyzer
            pkg-config
            protobuf # `protoc`: Signal's libraries compile protobufs at build time
          ];

          buildInputs = lib.optionals stdenv.hostPlatform.isLinux linuxLibs;

          env =
            {
              RUST_SRC_PATH = "${pkgs.rustToolchain}/lib/rustlib/src/rust/library";
              # As in packaging/nix/package.nix: link with cc, so the programs find their libraries.
              CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS = "-C linker-features=-lld";
              # Unoptimized, presage and libsignal need more than Rust's default 2 MiB of stack, and
              # chatd's signal-wire thread overflows it, which aborts chatd. chatd inherits this
              # from the window. Only the pages a thread touches use memory.
              RUST_MIN_STACK = toString (64 * 1024 * 1024);
            }
            // lib.optionalAttrs stdenv.hostPlatform.isLinux {
              LD_LIBRARY_PATH = lib.makeLibraryPath runtimeLibs;
              # WebKitGTK gets TLS from glib-networking's GIO module. NixOS has no system copy,
              # so without this the sign-in window can't load any https page.
              GIO_EXTRA_MODULES = "${pkgs.glib-networking}/lib/gio/modules";
            };

          # GTK aborts on a missing GSettings schema, and NixOS has none outside a package's own
          # share directory.
          shellHook = lib.optionalString stdenv.hostPlatform.isLinux ''
            export XDG_DATA_DIRS="${pkgs.gsettings-desktop-schemas}/share/gsettings-schemas/${pkgs.gsettings-desktop-schemas.name}:${pkgs.gtk3}/share/gsettings-schemas/${pkgs.gtk3.name}''${XDG_DATA_DIRS:+:$XDG_DATA_DIRS}"
          '';
        };
      }
    );
  };
}
