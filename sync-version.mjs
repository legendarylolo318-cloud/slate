// Syncs the version from package.json → tauri.conf.json + Cargo.toml.
// Runs automatically via the npm "version" lifecycle hook.

import { readFileSync, writeFileSync } from "fs";

const { version } = JSON.parse(readFileSync("package.json", "utf8"));

// tauri.conf.json
const tauri = readFileSync("src-tauri/tauri.conf.json", "utf8");
writeFileSync(
  "src-tauri/tauri.conf.json",
  tauri.replace(/"version": "[^"]+"/, `"version": "${version}"`)
);

// Cargo.toml (only the package version, not dependency versions)
const cargo = readFileSync("src-tauri/Cargo.toml", "utf8");
writeFileSync(
  "src-tauri/Cargo.toml",
  cargo.replace(/^version = "[^"]+"/m, `version = "${version}"`)
);

// metainfo.xml (release version + date)
const metainfo = readFileSync(
  "data/io.github.lullabyX.sone.metainfo.xml",
  "utf8"
);
const today = new Date().toISOString().slice(0, 10);
writeFileSync(
  "data/io.github.lullabyX.sone.metainfo.xml",
  metainfo.replace(
    /<release version="[^"]+" date="[^"]+">/,
    `<release version="${version}" date="${today}">`
  )
);

// PKGBUILD
try {
  const pkgbuild = readFileSync("build-scripts/build/PKGBUILD", "utf8");
  writeFileSync(
    "build-scripts/build/PKGBUILD",
    pkgbuild.replace(/^pkgver=.+$/m, `pkgver=${version}`)
  );
  console.log(
    `Synced version ${version} → tauri.conf.json, Cargo.toml, metainfo.xml, PKGBUILD`
  );
} catch (e) {
  if (e.code !== "ENOENT") throw e;
  console.log(
    `Synced version ${version} → tauri.conf.json, Cargo.toml, metainfo.xml`
  );
  console.warn("PKGBUILD not found, skipping");
}
