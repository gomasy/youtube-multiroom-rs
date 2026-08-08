const { Namer } = require("@parcel/plugin");
const path = require("node:path");

// An install keeps the manifest and the icons it names on the device and
// re-fetches them later, so those URLs must survive a deploy. Parcel already
// leaves index.html and the manifest under stable names; this adds the icon PNGs
// they point at. Everything else stays hashed, which is what src/static_files.rs
// serves as immutable.
//
// CommonJS because Parcel warns on every build for an .mjs plugin.
module.exports = new Namer({
  name({ bundle }) {
    const asset = bundle.getMainEntry();
    // null defers to the default namer, which appends the content hash.
    if (!asset || !isInstalledIcon(asset.filePath)) {
      return null;
    }
    return path.basename(asset.filePath);
  },
});

// The PNGs under src/icons/, and not the SVGs beside them: those are either
// sources the PNGs are generated from or ordinary app assets like logo.svg.
function isInstalledIcon(filePath) {
  const { dir, ext } = path.parse(filePath);
  return path.basename(dir) === "icons" && ext === ".png";
}
