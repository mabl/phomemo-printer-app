# Phomemo Printer Application — thin build orchestrator.
#
# Cargo builds the Rust static library, cbindgen generates the C header,
# cc compiles and links the PAPPL binary.

CC       ?= cc
CPPFLAGS ?=
CFLAGS   ?= -O2
LDFLAGS  ?=
LDLIBS   ?=
# Always applied, even when the environment overrides CFLAGS.
WARN_CFLAGS := -Wall -Wextra
CARGO    ?= cargo
CBINDGEN ?= cbindgen
PREFIX   ?= /usr/local
BINDIR   ?= $(PREFIX)/bin
SYSTEMD_DIR ?= /etc/systemd/system
ENV_DIR  ?= /etc/default

# Exported so cargo, cbindgen and this Makefile agree on where artifacts live.
# This deliberately overrides any `build.target-dir` from .cargo/config.toml:
# the paths below must match what cargo actually writes.
CARGO_TARGET_DIR ?= target
export CARGO_TARGET_DIR

# Pin to PAPPL 1.x — the driver uses 1.4 callback signatures.
# A clean migration to pappl2 (different ABI) should be a separate effort.
PAPPL_CFLAGS := $(shell pkg-config --cflags pappl 2>/dev/null)
PAPPL_LIBS   := $(shell pkg-config --libs   pappl 2>/dev/null)

RUST_LIB  := $(CARGO_TARGET_DIR)/release/libphomemo_pappl.a
# Native libraries the staticlib needs, written by rustc itself
# (--print native-static-libs) whenever it links the library.
RUST_NATIVE_LIBS_FILE := $(abspath $(CARGO_TARGET_DIR)/release/phomemo_pappl.native-libs)
CARGO_BUILD_LIB := $(CARGO) rustc --release --locked -p phomemo-pappl --lib -- \
                   --print native-static-libs=$(RUST_NATIVE_LIBS_FILE)

GEN_HDR   := generated/phomemo_pappl.h
BIN       := phomemo-printer-app
UNIT_FILE := systemd/phomemo-printer-app.service
ENV_FILE_EXAMPLE := systemd/phomemo-printer-app.env.example

C_SRCS    := c/main.c c/driver.c c/device_bt.c c/bridge.c c/media.c
C_FLAGS_ALL = $(WARN_CFLAGS) $(CPPFLAGS) $(CFLAGS) -I generated $(PAPPL_CFLAGS)
# Sources cbindgen reads, plus their directories so that adding, removing or
# renaming a module also regenerates the header.
PAPPL_CRATE_SRCS := $(shell find phomemo-pappl/src -name '*.rs')
PAPPL_CRATE_DIRS := $(shell find phomemo-pappl/src -type d)

.PHONY: all rust header clean test lint c-lint fmt fmt-check check FORCE \
        install uninstall install-systemd uninstall-systemd

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
$(BIN): $(RUST_LIB) $(GEN_HDR) $(C_SRCS)
	$(CC) $(C_FLAGS_ALL) $(LDFLAGS) -o $@ $(C_SRCS) \
	  $(RUST_LIB) $(PAPPL_LIBS) $$(cat "$(RUST_NATIVE_LIBS_FILE)") $(LDLIBS)

# Rust unit tests
test:
	$(CARGO) test --workspace --locked

lint:
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings

# Compile every C file with -Werror to a scratch output. Always runs, so C
# warnings fail `make check` even when the binary is already up to date.
c-lint: $(GEN_HDR)
	@set -e; for f in $(C_SRCS); do \
	  echo "$(CC) $(C_FLAGS_ALL) -Werror -c $$f -o /dev/null"; \
	  $(CC) $(C_FLAGS_ALL) -Werror -c $$f -o /dev/null; \
	done

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all --check

# Everything CI runs.
check: fmt-check lint test c-lint all

install: $(BIN)
	install -d "$(DESTDIR)$(BINDIR)"
	install -m 0755 "$(BIN)" "$(DESTDIR)$(BINDIR)/$(BIN)"

uninstall:
	rm -f "$(DESTDIR)$(BINDIR)/$(BIN)"

install-systemd: install
	install -d "$(DESTDIR)$(SYSTEMD_DIR)"
	install -m 0644 "$(UNIT_FILE)" "$(DESTDIR)$(SYSTEMD_DIR)/phomemo-printer-app.service"
	install -d "$(DESTDIR)$(ENV_DIR)"
	if [ ! -f "$(DESTDIR)$(ENV_DIR)/phomemo-printer-app" ]; then \
	  install -m 0644 "$(ENV_FILE_EXAMPLE)" "$(DESTDIR)$(ENV_DIR)/phomemo-printer-app"; \
	fi

uninstall-systemd:
	rm -f "$(DESTDIR)$(SYSTEMD_DIR)/phomemo-printer-app.service"

clean:
	$(CARGO) clean
	rm -rf generated $(BIN)
