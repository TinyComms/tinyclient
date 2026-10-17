# tinyclient for Nix: the four programs side by side in lib/tinyclient, as the .deb and .rpm install
# them (packaging/linux.sh), with the menu entry, the icons and the optional chatd user unit.
# flake.nix builds it from the checkout. The tests are CI's (`cargo test --workspace`).
{
  lib,
  makeRustPlatform,
  rustToolchain,
  pkg-config,
  protobuf,
  librsvg,
  wrapGAppsHook3,
  openssl,
  alsa-lib,
  glib,
  glib-networking,
  gsettings-desktop-schemas,
  gtk3,
  webkitgtk_4_1,
  libsoup_3,
  vulkan-loader,
  libGL,
  wayland,
  libxkbcommon,
  libx11,
  libxcb,
  fontconfig,
  freetype,
  # Telegram asks every client for an app id of its own (README, "Telegram needs your own app
  # id"). Without both, chatd lists Telegram as unavailable. Both end up in the Nix store.
  telegramApiId ? null,
  telegramApiHash ? null,
}: let
  rustPlatform = makeRustPlatform {
    cargo = rustToolchain;
    rustc = rustToolchain;
  };
  programs = ["tinyclient" "chatd" "mediad" "tinyclient-login"];
  # Opened with dlopen, so no NEEDED entry names them and the fixup would drop them from the
  # rpath: wgpu's Vulkan loader and its EGL fallback, the Wayland client (GPUI loads it only in a
  # Wayland session), and the X11 side of the window.
  runtimeLibs = [vulkan-loader libGL wayland libxkbcommon libx11 libxcb fontconfig freetype];
in
  rustPlatform.buildRustPackage {
    pname = "tinyclient";
    inherit ((lib.importTOML ../../Cargo.toml).workspace.package) version;

    # What the build reads, so an edit to the docs or the site does not rebuild it.
    src = lib.fileset.toSource {
      root = ../..;
      fileset = lib.fileset.unions [
        ../../Cargo.toml
        ../../Cargo.lock
        ../../crates
        ../../packaging
      ];
    };

    cargoLock = {
      lockFile = ../../Cargo.lock;
      # One per git repository in Cargo.lock, under the first crate it provides. When a rev there
      # moves, packaging/nix/update-hashes.sh rewrites them.
      outputHashes = {
        "collections-0.1.0" = "sha256-bgAan8bvIC7hAozTK+1fFQSicwqZczpWI4EyP/4hXFw="; # zed-industries/zed
        "curve25519-dalek-4.1.3" = "sha256-bPh7eEgcZnq9C3wmSnnYv0C4aAP+7pnwk9Io29GrI4A="; # signalapp/curve25519-dalek
        "libsignal-account-keys-0.1.0" = "sha256-Uh/j8cXUWgWgSo9UBfYOFuC8i+2YdMwGHcXf55PkGgU="; # signalapp/libsignal
        "libsignal-service-0.1.0" = "sha256-HiFXhs1tA9lAGED3QOLz/YStQn6Z9z7JC39p2CUJbws="; # whisperfish/libsignal-service-rs
        "presage-0.8.0-dev" = "sha256-bcXC2J6inrG6ZfakJqWMhrNmfZF6XfQzHJEOnNpHE90="; # whisperfish/presage
        "proptest-1.10.0" = "sha256-p5NTcHhruI8QQvANACg8AMRVNmuvGxs2NLit+/8PaWo="; # proptest-rs/proptest
        "spqr-1.5.1" = "sha256-XlqyjWQ5/F25/FdRTc4RDCqp8Gr1LCEBLeatXKnVciI="; # signalapp/SparsePostQuantumRatchet
        "tlottie-1.0.6" = "sha256-WtYyyf7AL+jtYY368X26PTnKSBUSZ9+IZjod/T5Oceg="; # dkaraush/tlottie
        "xim-ctext-0.3.0" = "sha256-pRT4Sz1JU9ros47/7pmIW9kosWOGMOItcnNd+VrvnpE="; # zed-industries/xim-rs
        "zed-font-kit-0.14.1-zed" = "sha256-KXygi0olNQi5yM8eaJVykNDtbPMDjT+cWPBF8UrtXR4="; # zed-industries/font-kit
        "zed-scap-0.0.8-zed" = "sha256-BihiQHlal/eRsktyf0GI3aSWsUCW7WcICMsC2Xvb7kw="; # zed-industries/scap
      };
    };

    # The release build of .github/workflows/release.yml.
    cargoBuildFlags = ["-p" "tinyclient" "--features" "tinyclient/gui" "-p" "chatd" "-p" "mediad" "-p" "login"];
    doCheck = false;

    nativeBuildInputs = [
      pkg-config
      protobuf # Signal's libraries compile protobufs at build time
      librsvg # rsvg-convert, for the icons
      wrapGAppsHook3
    ];

    buildInputs =
      runtimeLibs
      ++ [
        openssl
        alsa-lib
        glib
        gtk3
        webkitgtk_4_1
        libsoup_3
        # In the sign-in helper's wrapper: TLS for WebKitGTK, and the GSettings schemas GTK reads.
        glib-networking
        gsettings-desktop-schemas
      ];

    env =
      {
        # Rust links x86_64 Linux programs with its own rust-lld, past the Nix linker wrapper that
        # puts each library's directory in the rpath: chatd then cannot find libssl. This hands
        # linking back to cc.
        CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS = "-C linker-features=-lld";
      }
      // lib.optionalAttrs (telegramApiId != null) {TINYCLIENT_TELEGRAM_API_ID = toString telegramApiId;}
      // lib.optionalAttrs (telegramApiHash != null) {TINYCLIENT_TELEGRAM_API_HASH = telegramApiHash;};

    # The sign-in helper is the only GTK program; it is wrapped by itself in postFixup.
    dontWrapGApps = true;

    # Each program finds the others beside itself (`current_exe().with_file_name(..)`), which is
    # the real path behind the bin/ link.
    postInstall = ''
      mkdir -p $out/lib/tinyclient
      for program in ${toString programs}; do
        mv $out/bin/$program $out/lib/tinyclient/
      done
      ln -s ../lib/tinyclient/tinyclient $out/bin/tinyclient

      install -Dm644 packaging/linux/dev.tinyclient.app.desktop -t $out/share/applications
      mkdir -p $out/lib/systemd/user
      substitute packaging/linux/tinyclient-chatd.service $out/lib/systemd/user/tinyclient-chatd.service \
        --replace-fail /usr/lib/tinyclient/chatd $out/lib/tinyclient/chatd

      # As packaging/linux.sh draws them: 32 px and below from the drawing made for small sizes.
      for size in 16 22 24 32 48 64 128 256 512; do
        svg=packaging/icon.svg
        [ $size -le 32 ] && svg=packaging/icon-small.svg
        mkdir -p $out/share/icons/hicolor/''${size}x$size/apps
        rsvg-convert -w $size -h $size $svg -o $out/share/icons/hicolor/''${size}x$size/apps/dev.tinyclient.app.png
      done
      install -Dm644 packaging/icon.svg $out/share/icons/hicolor/scalable/apps/dev.tinyclient.app.svg
    '';

    postFixup = ''
      for program in ${toString programs}; do
        patchelf --add-rpath ${lib.makeLibraryPath runtimeLibs} $out/lib/tinyclient/$program
      done
      wrapGApp $out/lib/tinyclient/tinyclient-login
    '';

    meta = {
      description = "Slack, Discord, Telegram, Teams, Signal and Messages in one small native window";
      homepage = "https://tinyclient.io";
      # chatd links Signal's libraries, so as built here it is a combined work under the AGPL
      # (README, "Signal and the AGPL"). The rest is MIT.
      license = with lib.licenses; [mit agpl3Only];
      mainProgram = "tinyclient";
      platforms = lib.platforms.linux;
    };
  }
