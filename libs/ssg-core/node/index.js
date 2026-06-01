// Loader for the `treaty_ssg_node` NAPI addon: the thin native bridge that lets
// JS (`@treaty/ssg`) drive the pure Rust SSG core (`treaty_ssg`). It resolves the
// platform-specific prebuilt `.node` next to this file. All SSG logic lives in
// Rust; this file only loads the binary and re-exports its functions.
const { existsSync } = require('fs')
const { join } = require('path')

const { platform, arch } = process

/** Candidate `.node` filenames for the current platform/arch, most-specific first. */
function candidates() {
  switch (platform) {
    case 'win32':
      return arch === 'x64'
        ? ['treaty_ssg_node.win32-x64-msvc.node']
        : [`treaty_ssg_node.win32-${arch}-msvc.node`]
    case 'darwin':
      return [`treaty_ssg_node.darwin-${arch}.node`]
    case 'linux':
      return [`treaty_ssg_node.linux-${arch}-gnu.node`, `treaty_ssg_node.linux-${arch}-musl.node`]
    default:
      return []
  }
}

let nativeBinding = null
let loadError = null

for (const name of candidates()) {
  const local = join(__dirname, name)
  if (existsSync(local)) {
    try {
      nativeBinding = require(local)
      break
    } catch (err) {
      loadError = err
    }
  }
}

if (!nativeBinding) {
  if (loadError) throw loadError
  throw new Error(`Failed to load the treaty_ssg_node native binding for ${platform}-${arch}`)
}

module.exports.generateSite = nativeBinding.generateSite
module.exports.prerenderSite = nativeBinding.prerenderSite
