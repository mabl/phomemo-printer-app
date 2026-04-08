# Phomemo Printer Application — thin build orchestrator.
#
# Cargo builds the Rust static library, cbindgen generates the C header,
# cc compiles and links the PAPPL binary.

CC       ?= cc
CFLAGS   ?= -Wall -Wextra -O2
LDFLAGS  ?=
PREFIX   ?= /usr/local
BINDIR   ?= $(PREFIX)/bin
SYSTEMD_DIR ?= /etc/systemd/system
ENV_DIR  ?= /etc/default

# Pin to PAPPL 1.x — the driver uses 1.4 callback signatures.
# A clean migration to pappl2 (different ABI) should be a separate effort.
PAPPL_CFLAGS := $(shell pkg-config --cflags pappl 2>/dev/null)
PAPPL_LIBS   := $(shell pkg-config --libs   pappl 2>/dev/null)

RUST_LIB  := target/release/libphomemo_pappl.a
RUST_NATIVE_LIBS := -ldl -lpthread -lm
GEN_HDR   := generated/phomemo_pappl.h
BIN       := phomemo-printer-app
UNIT_FILE := systemd/phomemo-printer-app.service
ENV_FILE_EXAMPLE := systemd/phomemo-printer-app.env.example

C_SRCS    := c/main.c c/driver.c c/device_bt.c c/bridge.c c/media.c

.PHONY: all rust header clean test lint install uninstall install-systemd uninstall-systemd

all: $(BIN)

# 1. Build Rust staticlib
rust:
	cargo build --release -p phomemo-pappl

# 2. Generate C header (explicit Make step, NOT build.rs)
$(GEN_HDR): phomemo-pappl/src/lib.rs phomemo-pappl/src/ffi.rs phomemo-pappl/cbindgen.toml
	@mkdir -p generated
	cbindgen --config phomemo-pappl/cbindgen.toml \
	         --crate phomemo-pappl \
	         --output $@

header: $(GEN_HDR)

# 3. Compile and link
$(BIN): rust header $(C_SRCS)
	$(CC) $(CFLAGS) -o $@ $(C_SRCS) \
	  -I generated \
	  $(PAPPL_CFLAGS) \
	  $(RUST_LIB) \
	  $(PAPPL_LIBS) \
	  $(RUST_NATIVE_LIBS) \
	  $(LDFLAGS)

# Rust unit tests
test:
	cargo test --workspace

lint:
	cargo clippy --workspace --all-targets -- -D warnings

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
	cargo clean
	rm -rf generated $(BIN)
