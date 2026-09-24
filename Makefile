PLUGIN_DIR := com.esteban-dcp.claudecodeusage.sdPlugin
BUNDLE_ID := $(basename $(PLUGIN_DIR))
PACKAGE := oaclaudecode-usage

UNAME_S := $(shell uname -s)
ifeq ($(UNAME_S),Darwin)
	TRIPLE := $(shell uname -m)-apple-darwin
	OPENDECK_CONFIG := $(HOME)/Library/Application Support/OpenDeck
else
	TRIPLE := $(shell uname -m)-unknown-linux-gnu
	OPENDECK_CONFIG := $(or $(XDG_CONFIG_HOME),$(HOME)/.config)/opendeck
endif

BINARY := $(PACKAGE)-$(TRIPLE)

.PHONY: all build stage package install clean

all: build

build:
	cargo build --release
	mkdir -p stage
	cp target/release/$(PACKAGE) stage/$(BINARY)

stage: build
	mkdir -p $(PLUGIN_DIR)/actions
	cp target/release/$(PACKAGE) $(PLUGIN_DIR)/$(BINARY)
	cp assets/manifest.json $(PLUGIN_DIR)/manifest.json
	cp assets/pi.html $(PLUGIN_DIR)/pi.html
	cp assets/icon.svg $(PLUGIN_DIR)/icon.svg
	cp assets/claude.png $(PLUGIN_DIR)/claude.png
	cp assets/actions/icon.svg $(PLUGIN_DIR)/actions/icon.svg

package: stage
	rm -f $(BUNDLE_ID).zip
	zip -rq $(BUNDLE_ID).zip $(PLUGIN_DIR)

install: stage
	mkdir -p "$(OPENDECK_CONFIG)/plugins"
	cp -r $(PLUGIN_DIR) "$(OPENDECK_CONFIG)/plugins/"

clean:
	cargo clean
	rm -rf stage $(PLUGIN_DIR) $(BUNDLE_ID).zip