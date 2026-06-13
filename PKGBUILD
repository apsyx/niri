# Maintainer: zhenggq

pkgname=niri-plus
pkgver=25.11.r0.e88c1fb4
pkgrel=2
pkgdesc="A scrollable-tiling Wayland compositor (with color management & HDR support)"
arch=(x86_64)
url="https://github.com/apsyx/niri"
license=(GPL-3.0-or-later)
depends=(
  cairo
  gcc-libs
  glib2
  glibc
  lcms2
  libdisplay-info
  libinput
  libpipewire
  libxkbcommon
  mesa
  pango
  pixman
  seatd
  systemd-libs
  xdg-desktop-portal-impl
)
makedepends=(
  clang
  git
  rust
)
optdepends=(
  'alacritty: a suggested GPU-accelerated terminal emulator'
  'bash: for niri-session script'
  'fuzzel: a suggested Wayland application launcher'
  'mako: a suggested Wayland notification daemon'
  'org.freedesktop.secrets: for apps to rely on secrets portal'
  'swaybg: a suggested Wayland wallpaper tool'
  'swaylock: a suggested Wayland screen locker'
  'waybar: a suggested Wayland customizable desktop bar'
  'xwayland-satellite: for running X11 apps in XWayland'
  'xdg-desktop-portal-gtk: a suggested XDG desktop portal'
  'xdg-desktop-portal-gnome: a XDG desktop portal required for screencasting'
)
provides=(niri wayland-compositor)
conflicts=(niri)
source=(
  "git+https://github.com/apsyx/niri.git#branch=color-management-hdr"
)
sha256sums=('SKIP')

pkgver() {
  cd niri
  printf "25.11.r%s.%s" "$(git rev-list v25.11..HEAD --count 2>/dev/null || echo 0)" "$(git rev-parse --short HEAD)"
}

prepare() {
  cd niri
  cargo fetch --target "$(rustc --print host-tuple)"
}

build() {
  cd niri
  export NIRI_BUILD_COMMIT="$(git rev-parse --short HEAD)"
  CFLAGS+=(' -ffat-lto-objects')
  cargo build --release --features default

  # generate shell completions
  for shell in bash fish zsh; do
    cargo run --release --bin niri -- \
      completions "$shell" > "$shell-completions"
  done
}

check() {
  cd niri
  export XDG_RUNTIME_DIR="$(mktemp -d)"
  export RAYON_NUM_THREADS=1
  cargo test --all --exclude niri-visual-tests
}

package() {
  cd niri
  install -vDm 755 {target/release/niri,resources/niri-session} -t "$pkgdir/usr/bin/"
  install -vDm 644 resources/niri{.service,-shutdown.target} -t "$pkgdir/usr/lib/systemd/user/"
  install -vDm 644 resources/niri.desktop -t "$pkgdir/usr/share/wayland-sessions/"
  install -vDm 644 resources/niri-portals.conf -t "$pkgdir/usr/share/xdg-desktop-portal/"
  install -vDm 644 resources/default-config.kdl README.md -t "$pkgdir/usr/share/doc/niri/"
  # shell auto-completions
  install -vDm 644 bash-completions "$pkgdir/usr/share/bash-completion/completions/niri"
  install -vDm 644 fish-completions "$pkgdir/usr/share/fish/vendor_completions.d/niri.fish"
  install -vDm 644 zsh-completions "$pkgdir/usr/share/zsh/site-functions/_niri"
}
