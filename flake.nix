{
  description = "Logos QR scanner: shows QR codes and reads them through the camera for other modules, via the Logos QR helper it bundles.";

  inputs = {
    logos-module-builder.url = "github:logos-co/logos-module-builder";
  };

  outputs = inputs@{ self, logos-module-builder, ... }:
    let
      nixpkgs = logos-module-builder.inputs.nixpkgs;
      lib = nixpkgs.lib;
      systems = [ "aarch64-darwin" "x86_64-darwin" "aarch64-linux" "x86_64-linux" ];
      # x86_64-windows is a cross target the builder understands, never a host.
      targets = systems ++ [ "x86_64-windows" ];
      pin = lib.importJSON ./helper.json;
      pkgsFor = system: import nixpkgs { inherit system; };

      # The helper from source, for tests and development. The module ships a released helper
      # (helper.json) where one is pinned for the system: built and signed once, never here.
      helper = system:
        let pkgs = pkgsFor system;
        in pkgs.rustPlatform.buildRustPackage {
          pname = "logos-qr-helper";
          version = pin.version;
          src = ./helper;
          cargoLock.lockFile = ./helper/Cargo.lock;
          nativeBuildInputs = [ pkgs.cmake pkgs.pkg-config ];
          # SDL finds its video and camera backends here; it loads them at run time.
          buildInputs = lib.optionals pkgs.stdenv.isLinux (with pkgs; [
            xorg.libX11 xorg.libXext xorg.libXcursor xorg.libXi xorg.libXrandr xorg.libXScrnSaver
            wayland wayland-scanner libxkbcommon libdecor pipewire libGL
          ]);
          # With the fake device beside it, for doctests; only logos-qr-helper is ever shipped.
          buildFeatures = [ "fake-device" ];
          dontUseCmakeConfigure = true;
          doCheck = false;
          meta.mainProgram = "logos-qr-helper";
        };

      qrPresent = system:
        let pkgs = pkgsFor system;
        in pkgs.runCommand "qr-present" { meta.mainProgram = "qr-present"; } ''
          mkdir -p $out/bin
          cp ${./tools/qr-present} $out/bin/qr-present
          chmod +x $out/bin/qr-present
        '';

      # On macOS the helper is an app of its own, so the camera prompt names it. A development
      # build gets the .dev bundle id and an ad hoc signature, and never the release's grant.
      helperApp = system:
        let pkgs = pkgsFor system; bin = helper system;
        in pkgs.runCommand "logos-qr-helper-app-${pin.version}" { nativeBuildInputs = [ pkgs.darwin.sigtool pkgs.darwin.cctools ]; } ''
          app=$out/LogosQR.app
          mkdir -p $app/Contents/MacOS
          cp ${bin}/bin/logos-qr-helper $app/Contents/MacOS/logos-qr-helper
          chmod u+w $app/Contents/MacOS/logos-qr-helper
          sed -e 's|@BUNDLE_ID@|co.logos.QRHelper.dev|g' -e 's|@NAME@|Logos QR (dev)|g' -e 's|@VERSION@|${pin.version}|g' \
            ${./helper/macos/Info.plist.in} > $app/Contents/Info.plist
          codesign -f -s - -i co.logos.QRHelper.dev --entitlements ${./helper/macos/LogosQR.entitlements} \
            $app/Contents/MacOS/logos-qr-helper
        '';

      # What the module carries beside its plugin, per system: the pinned release, else a
      # development build. Nothing yet for Windows.
      released = system: pin.assets.${system} or null;
      payload = system:
        let
          pkgs = pkgsFor system;
          asset = released system;
        in
        if asset != null then
          pkgs.runCommand "logos-qr-helper-release-${pin.version}" { } ''
            mkdir -p $out
            tar -xzf ${pkgs.fetchurl { inherit (asset) url sha256; }} -C $out
          ''
        else if lib.hasSuffix "darwin" system then helperApp system
        else pkgs.runCommand "logos-qr-helper-dev-${pin.version}" { } ''
          mkdir -p $out && cp ${helper system}/bin/logos-qr-helper $out/
        '';

      module = logos-module-builder.lib.mkLogosModule {
        src = ./.;
        configFile = ./metadata.json;
        flakeInputs = inputs;
        # After fixup, so strip and the darwin re-signing hook never touch the signed helper.
        postFixup = { pkgs, system }:
          lib.optionalString (builtins.elem system systems) ''
            cp -R ${payload system}/. $out/lib/
          '';
      };
    in
    {
      packages = lib.genAttrs targets (system:
        module.packages.${system}
        // lib.optionalAttrs (builtins.elem system systems) {
          helper = helper system;
          helper-payload = payload system;
          fake-device = helper system;
          qr-present = qrPresent system;
        });
    };
}
