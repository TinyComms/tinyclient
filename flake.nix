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
  };
}
