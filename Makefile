SWITCHER    := cosmic-windows-swapper
APPLET      := cosmic-applet-windows-swapper
INSTALL_DIR := $(HOME)/.local/bin
APPS_DIR    := $(HOME)/.local/share/applications
SWITCHER_BIN := target/release/$(SWITCHER)
APPLET_BIN  := target/release/$(APPLET)
DESKTOP     := applet-windows-swapper/data/io.github.wolfhound389.CosmicWindowsSwapper.applet.desktop
ICON_SRC    := applet-windows-swapper/data/io.github.wolfhound389.CosmicWindowsSwapper.applet-symbolic.svg
ICONS_DIR   := $(HOME)/.local/share/icons/hicolor/scalable/apps

.PHONY: all build install uninstall enable disable status reinstall check-compat

all: build

build:
	@command -v cargo >/dev/null 2>&1 || { \
		echo "Rust not found. Install with:"; \
		echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"; \
		exit 1; \
	}
	cargo build --release --workspace

install: build
	@mkdir -p $(INSTALL_DIR) $(APPS_DIR) $(ICONS_DIR)
	install -Dm755 $(SWITCHER_BIN) $(INSTALL_DIR)/$(SWITCHER)
	install -Dm755 $(APPLET_BIN) $(INSTALL_DIR)/$(APPLET)
	install -Dm644 $(DESKTOP) $(APPS_DIR)/io.github.wolfhound389.CosmicWindowsSwapper.applet.desktop
	install -Dm644 $(ICON_SRC) $(ICONS_DIR)/io.github.wolfhound389.CosmicWindowsSwapper.applet-symbolic.svg
	@$(MAKE) enable
	@echo ""
	@echo "Installed and enabled. Press Super+Tab or Alt+Tab to try it."
	@echo "Add the 'Windows Swapper Settings' applet to your COSMIC panel to change themes."

uninstall:
	@$(MAKE) disable
	@rm -f $(INSTALL_DIR)/$(SWITCHER)
	@rm -f $(INSTALL_DIR)/$(APPLET)
	@rm -f $(APPS_DIR)/io.github.wolfhound389.CosmicWindowsSwapper.applet.desktop
	@rm -f $(ICONS_DIR)/io.github.wolfhound389.CosmicWindowsSwapper.applet-symbolic.svg
	@echo "Uninstalled. COSMIC default switcher restored."

enable:
	@bash scripts/enable.sh

disable:
	@bash scripts/disable.sh

status:
	@bash scripts/status.sh

reinstall: uninstall install

check-compat:
	@echo "Checking compatibility..."
	@bash scripts/find-config.sh > /dev/null 2>&1; case $$? in \
		0) echo "  COSMIC shortcuts config: found" ;; \
		2) echo "  COSMIC shortcuts config: not created yet (run 'make enable')" ;; \
		*) echo "  COSMIC shortcuts config: NOT found" ;; \
	esac
	@test -f $(INSTALL_DIR)/$(SWITCHER) && echo "  Switcher binary: installed" || echo "  Switcher binary: not installed"
	@test -f $(INSTALL_DIR)/$(APPLET)   && echo "  Applet binary:   installed" || echo "  Applet binary:   not installed"
	@command -v cosmic-comp >/dev/null 2>&1 && echo "  cosmic-comp: found" || echo "  cosmic-comp: not found (is COSMIC running?)"
	@echo "Done."
