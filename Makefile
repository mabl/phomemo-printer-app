# Phomemo Printer Application — thin build orchestrator.
#
# Cargo builds the Rust static library, cbindgen generates the C header,
# cc compiles and links the PAPPL binary.
#
# The install targets only copy what `make` built, so they work as root
# without a Rust toolchain: build first, then install.

CC       ?= cc
CPPFLAGS ?=
CFLAGS   ?= -O2
LDFLAGS  ?=
LDLIBS   ?=
CARGO    ?= cargo
CBINDGEN ?= cbindgen
PKG_CONFIG ?= pkg-config
INSTALL  ?= install

# Installation directories, all below DESTDIR when it is set. UNITDIR is
# where systemd looks for an administrator's units under /usr/local, and a
# distribution's under /usr; ENVFILE is the service's configuration.
PREFIX     ?= /usr/local
BINDIR     ?= $(PREFIX)/bin
DATADIR    ?= $(PREFIX)/share
UNITDIR    ?= $(PREFIX)/lib/systemd/system
SYSCONFDIR ?= /etc
ENVFILE    ?= $(SYSCONFDIR)/default/phomemo-printer-app

# Exported so cargo, cbindgen and this Makefile agree on where artifacts live.
# This deliberately overrides any `build.target-dir` from .cargo/config.toml:
# the paths below must match what cargo actually writes.
CARGO_TARGET_DIR ?= target
export CARGO_TARGET_DIR

# The application's version: phomemo-pappl's, so that its Cargo.toml is the
# one place it is set. `cargo pkgid` prints a package ID ending in "#0.1.0"
# (or "#phomemo-pappl@0.1.0"). A literal "#" needs a variable to work with
# GNU make before and after 4.3. Looked up once, when first needed, so that
# targets which compile nothing do not run cargo.
HASH    := \#
VERSION  = $(eval VERSION := $(lastword $(subst @, ,$(subst $(HASH), ,$(shell \
             $(CARGO) pkgid --offline -p phomemo-pappl)))))$(VERSION)

RUST_LIB  := $(CARGO_TARGET_DIR)/release/libphomemo_pappl.a
# Native libraries the staticlib needs, written by rustc itself
# (--print native-static-libs) whenever it links the library.
RUST_NATIVE_LIBS_FILE := $(abspath $(CARGO_TARGET_DIR)/release/phomemo_pappl.native-libs)
CARGO_BUILD_LIB := $(CARGO) rustc --release --locked -p phomemo-pappl --lib -- \
                   --print native-static-libs=$(RUST_NATIVE_LIBS_FILE)

GEN_HDR   := generated/phomemo_pappl.h
BIN       := phomemo-printer-app
UNIT      := phomemo-printer-app.service
UNIT_IN   := systemd/$(UNIT).in
ROOT_DROPIN := systemd/root.conf
ENV_EXAMPLE := systemd/phomemo-printer-app.env.example

C_SRCS    := c/main.c c/driver.c c/device_bt.c c/media.c
C_HDRS    := c/phomemo.h
C_LINT    := $(C_SRCS:c/%.c=c-lint-%)
# Always applied, even when the environment overrides CPPFLAGS or CFLAGS:
# the language and POSIX interfaces the sources are written to, the warnings
# they are kept clean of, and the version.
STD_CFLAGS  := -std=c17 -D_POSIX_C_SOURCE=200809L
WARN_CFLAGS := -Wall -Wextra -Wpedantic -Wshadow -Wconversion -Wformat=2 \
               -Wstrict-prototypes -Wmissing-prototypes -Wvla
C_FLAGS_ALL = $(STD_CFLAGS) $(WARN_CFLAGS) -DPHOMEMO_VERSION='"$(VERSION)"' \
              $(CPPFLAGS) $(CFLAGS) -I generated $(PAPPL_CFLAGS)
# PAPPL 1.x - the driver uses the 1.4 callback signatures; a migration to
# pappl2 (a different ABI) is a separate effort. Looked up only for the
# goals that compile C, so that the others need neither PAPPL nor
# pkg-config, and an error, with pkg-config's own explanation, if missing.
PAPPL_PKG := pappl >= 1.4
C_GOALS   := all $(BIN) c-lint $(C_LINT) ci check
ifneq ($(filter $(C_GOALS),$(or $(MAKECMDGOALS),all)),)
  ifneq ($(shell $(PKG_CONFIG) --print-errors --exists '$(PAPPL_PKG)' && echo found),found)
    $(error $(PAPPL_PKG) not found by $(PKG_CONFIG): install PAPPL's development \
            files, or point PKG_CONFIG_PATH at them)
  endif
  PAPPL_CFLAGS := $(shell $(PKG_CONFIG) --cflags '$(PAPPL_PKG)')
  PAPPL_LIBS   := $(shell $(PKG_CONFIG) --libs '$(PAPPL_PKG)')
endif

# Sources cbindgen reads, plus their directories so that adding, removing or
# renaming a module also regenerates the header.
PAPPL_CRATE_SRCS := $(shell find phomemo-pappl/src -name '*.rs')
PAPPL_CRATE_DIRS := $(shell find phomemo-pappl/src -type d)

.PHONY: all rust header clean test lint c-lint $(C_LINT) fmt fmt-check ci check FORCE \
        install uninstall install-unit install-systemd uninstall-systemd

all: $(BIN)

# 1. Build Rust staticlib. Cargo always runs (it is incremental and tracks the
#    whole dependency graph) but only touches the .a when something changed,
#    so the link step below re-runs only when the library really changed.
#    Side effect of FORCE: `make -q` / `make -n` always report work to do.
#    rustc only writes the native-libs file when it actually links the crate,
#    so if the file went missing while the lib is fresh, clean just this
#    crate and rebuild it.
$(RUST_LIB): FORCE
	$(CARGO_BUILD_LIB)
	@test -f "$(RUST_NATIVE_LIBS_FILE)" || { \
	  $(CARGO) clean --release -p phomemo-pappl && $(CARGO_BUILD_LIB); }

rust: $(RUST_LIB)

# 2. Generate C header (explicit Make step, NOT build.rs). cbindgen leaves an
#    unchanged header untouched, so touch it to record that it is up to date.
$(GEN_HDR): $(PAPPL_CRATE_SRCS) $(PAPPL_CRATE_DIRS) \
            phomemo-pappl/cbindgen.toml phomemo-pappl/Cargo.toml
	@mkdir -p $(@D)
	$(CBINDGEN) --quiet \
	            --config phomemo-pappl/cbindgen.toml \
	            --crate phomemo-pappl \
	            --output $@
	@touch $@

header: $(GEN_HDR)

# 3. Compile and link
$(BIN): $(RUST_LIB) $(GEN_HDR) $(C_SRCS) $(C_HDRS)
	$(CC) $(C_FLAGS_ALL) $(LDFLAGS) -o $@ $(C_SRCS) \
	  $(RUST_LIB) $(PAPPL_LIBS) $$(cat "$(RUST_NATIVE_LIBS_FILE)") $(LDLIBS)

# Rust unit tests
test:
	$(CARGO) test --workspace --locked

lint:
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings

# Compile every C file with the build's flags plus -Werror to a scratch
# output. Phony, so C warnings fail `make check` even when the binary is
# already up to date.
c-lint: $(C_LINT)

$(C_LINT): c-lint-%: c/%.c $(C_HDRS) $(GEN_HDR)
	$(CC) $(C_FLAGS_ALL) -Werror -c $< -o /dev/null

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all --check

# What CI runs besides `nix flake check`, which builds the package; and
# that plus the build, to run before pushing.
ci: fmt-check lint test c-lint
check: ci all

# Installation. These targets build nothing - the binary's prerequisites
# would run cargo, as root under sudo - so `install` refuses to run before
# `make` has.
install:
	@test -x "$(BIN)" || { echo "$(BIN) is not built: run 'make' first." >&2; exit 1; }
	$(INSTALL) -d "$(DESTDIR)$(BINDIR)"
	$(INSTALL) -m 0755 "$(BIN)" "$(DESTDIR)$(BINDIR)/$(BIN)"

uninstall:
	rm -f "$(DESTDIR)$(BINDIR)/$(BIN)"

# The systemd unit, made from its template with this installation's paths,
# which go in as sed replacements (& and the delimiter | escaped) into a
# unit file (systemd's specifier character % doubled). systemd would split
# them at whitespace and unescape backslashes, so those are refused.
unit_path = $(subst %,%%,$(subst |,\|,$(subst &,\&,$(1))))

install-unit:
	@case "$(BINDIR)$(DATADIR)$(ENVFILE)" in *[[:space:]\\]*) \
	  echo "BINDIR, DATADIR and ENVFILE cannot contain whitespace or backslashes." >&2; \
	  exit 1;; esac
	$(INSTALL) -d "$(DESTDIR)$(UNITDIR)"
	sed -e 's|@BINDIR@|$(call unit_path,$(BINDIR))|g' \
	    -e 's|@DATADIR@|$(call unit_path,$(DATADIR))|g' \
	    -e 's|@ENVFILE@|$(call unit_path,$(ENVFILE))|g' \
	  "$(UNIT_IN)" > "$(DESTDIR)$(UNITDIR)/$(UNIT)"
	chmod 0644 "$(DESTDIR)$(UNITDIR)/$(UNIT)"
	$(INSTALL) -d "$(DESTDIR)$(DATADIR)/phomemo-printer-app"
	$(INSTALL) -m 0644 "$(ROOT_DROPIN)" "$(DESTDIR)$(DATADIR)/phomemo-printer-app/root.conf"

# The service: binary, unit, and a configuration file to edit, which is
# never overwritten.
install-systemd: install install-unit
	@if [ -e "$(DESTDIR)$(ENVFILE)" ]; then \
	  echo "Keeping the existing $(DESTDIR)$(ENVFILE)."; \
	else \
	  echo "Installing $(DESTDIR)$(ENVFILE)."; \
	  $(INSTALL) -d "$(DESTDIR)$(dir $(ENVFILE))" && \
	  $(INSTALL) -m 0644 "$(ENV_EXAMPLE)" "$(DESTDIR)$(ENVFILE)"; \
	fi
	@if [ "$(UNITDIR)" != /etc/systemd/system ] && \
	    [ -e "$(DESTDIR)/etc/systemd/system/$(UNIT)" ]; then \
	  echo "Warning: $(DESTDIR)/etc/systemd/system/$(UNIT), which earlier" \
	       "versions installed, overrides this unit: see Upgrading in" \
	       "README.md." >&2; \
	fi
	@test -n "$(DESTDIR)" || printf '%s\n' "" \
	  "Configure the service in $(ENVFILE), then start it:" \
	  "  systemctl daemon-reload" \
	  "  systemctl enable --now $(UNIT)"

# Removes what install-systemd installed except the configuration file,
# which may have been edited; disable the service first.
uninstall-systemd: uninstall
	rm -f "$(DESTDIR)$(UNITDIR)/$(UNIT)" "$(DESTDIR)$(DATADIR)/phomemo-printer-app/root.conf"
	@rmdir "$(DESTDIR)$(DATADIR)/phomemo-printer-app" 2>/dev/null || :
	@echo "Kept $(DESTDIR)$(ENVFILE) and the state in /var/lib/phomemo-printer-app."

clean:
	$(CARGO) clean
	rm -rf generated $(BIN)
