rootdir := ''
prefix := '/usr'
cargo-target-dir := env('CARGO_TARGET_DIR', 'target')
orca := '/usr/bin/orca'
usrdir := absolute_path(clean(rootdir / prefix))
bindir := usrdir / 'bin'
systemddir := usrdir / 'lib' / 'systemd' / 'user'
sessiondir := usrdir / 'share' / 'wayland-sessions'
applicationdir := usrdir / 'share' / 'applications'

default: build-release

build-debug *args:
    ORCA={{ orca }} cargo build {{ args }}

# Compile with release profile
build-release *args: (build-debug '--release' args)

# Compile with a vendored tarball
build-vendored *args: vendor-extract (build-release '--frozen --offline' args)

# Remove Cargo build artifacts
clean:
    cargo clean

# Also remove .cargo and vendored dependencies
clean-dist: clean
    rm -rf .cargo vendor vendor.tar target

# Installs files into the system
install:
    # main binary
    install -Dm0755 {{ cargo-target-dir }}/release/cosmic-session {{ bindir }}/cosmic-session

    # session start script
    install -Dm0755 data/start-cosmic {{ bindir }}/start-cosmic

    # systemd target
    install -Dm0644 data/cosmic-session.target {{ systemddir }}/cosmic-session.target

    # session
    install -Dm0644 data/cosmic.desktop {{ sessiondir }}/cosmic.desktop

    # mimeapps
    install -Dm0644 data/cosmic-mimeapps.list {{ applicationdir }}/cosmic-mimeapps.list

# Vendor Cargo dependencies locally
vendor:
    mkdir -p .cargo
    cargo vendor | head -n -1 > .cargo/config.toml
    echo 'directory = "vendor"' >> .cargo/config.toml
    tar pcf vendor.tar vendor
    rm -rf vendor

# Extracts vendored dependencies
[private]
vendor-extract:
    rm -rf vendor
    tar pxf vendor.tar

# Bump cargo version, create git commit, and create tag
tag version:
    find -type f -name Cargo.toml -exec sed -i '0,/^version/s/^version.*/version = "{{ version }}"/' '{}' \; -exec git add '{}' \;
    cargo check
    cargo clean
    git add Cargo.lock
    git commit -m 'release: {{ version }}'
    git commit --amend
    git tag -a {{ version }} -m ''
