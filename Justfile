rootdir := ''
etcdir := '/etc'
prefix := '/usr'
clean := '0'
debug := '0'
vendor := '0'
cargo-target-dir := env('CARGO_TARGET_DIR', 'target')
target := if debug == '1' { 'debug' } else { 'release' }
vendor_args := if vendor == '1' { '--frozen --offline' } else { '' }
debug_args := if debug == '1' { '' } else { '--release' }
cargo_args := vendor_args + ' ' + debug_args
orca := '/usr/bin/orca'
cosmic_dconf_profile := prefix + '/share/dconf/profile/cosmic'

bindir := rootdir / prefix + '/bin'
systemddir := rootdir / prefix + '/lib/systemd/user'
sessiondir := rootdir / prefix + '/share/wayland-sessions'
applicationdir := rootdir / prefix + '/share/applications'

all: _extract_vendor build

build:
        ORCA={{orca}} cargo build {{cargo_args}}

# Installs files into the system
install:
	echo {{cosmic_dconf_profile}}
	# main binary
	install -Dm0755 {{cargo-target-dir}}/release/cosmic-session {{bindir}}/cosmic-session

	# session start script
	install -Dm0755 data/start-cosmic {{bindir}}/start-cosmic
	sed -i "s|DCONF_PROFILE=cosmic|DCONF_PROFILE={{cosmic_dconf_profile}}|" {{bindir}}/start-cosmic

	# systemd target
	install -Dm0644 data/cosmic-session.target {{systemddir}}/cosmic-session.target

	# session
	install -Dm0644 data/cosmic.desktop {{sessiondir}}/cosmic.desktop

	# mimeapps
	install -Dm0644 data/cosmic-mimeapps.list {{applicationdir}}/cosmic-mimeapps.list

	# dconf profile
	install -Dm644 data/dconf/profile/cosmic {{rootdir}}/{{cosmic_dconf_profile}}

clean_vendor:
	rm -rf vendor vendor.tar .cargo/config

clean: clean_vendor
	cargo clean

# Extracts vendored dependencies if vendor=1
_extract_vendor:
	#!/usr/bin/env sh
	if test {{vendor}} = 1; then
		rm -rf vendor; tar pxf vendor.tar
	fi
