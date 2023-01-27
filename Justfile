rootdir := ''
etcdir := rootdir + '/etc'
prefix := rootdir + '/usr'
clean := '0'
debug := '0'
vendor := '0'
target := if debug == '1' { 'debug' } else { 'release' }
vendor_args := if vendor == '1' { '--frozen --offline' } else { '' }
debug_args := if debug == '1' { '' } else { '--release' }
cargo_args := vendor_args + ' ' + debug_args

bindir := prefix + '/bin'
systemddir := prefix + '/lib/systemd/user'
sessiondir := prefix + '/share/wayland-sessions'

all: _extract_vendor
	cargo build {{cargo_args}}

# Installs files into the system
install:
	#!/usr/bin/env sh
	
	# main binary
	install -Dm0755 target/release/cosmic-session {{bindir}}/cosmic-session

	# session start script
	install -Dm0755 data/start-cosmic {{bindir}}/start-cosmic
	
	# systemd target
	install -Dm0644 data/cosmic-session.target {{systemddir}}/cosmic-session.target
	
	# session	
	install -Dm0644 data/cosmic.desktop {{sessiondir}}/cosmic.desktop

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
