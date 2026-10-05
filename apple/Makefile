# dl-nzb for Mac, iPhone and iPad.
#
#   make kit-test      DlNzbKit's Swift Testing suites (models, stores, formatting)
#   make kit-build     DlNzbKit for macOS, and for the iOS simulator through xcodebuild
#   make xcframework   the Rust engine (DlNzbCore.xcframework), its Swift bindings and licences
#   make ios-run       the iPhone app in the simulator (see "iPhone and iPad" below)
#   make mac-run       the Mac app, built and opened (see "Mac" below)
#
# Every recipe runs under bash with `set -euo pipefail`, so a failing command
# in the middle of a recipe fails the target.
SHELL := /usr/bin/env bash
.SHELLFLAGS := -euo pipefail -c

APPLE_DIR := $(patsubst %/,%,$(dir $(abspath $(lastword $(MAKEFILE_LIST)))))
REPO_ROOT := $(patsubst %/,%,$(dir $(APPLE_DIR)))
KIT := $(APPLE_DIR)/DlNzbKit
# xcodebuild's products for the kit stay inside the package's ignored .build.
KIT_DERIVED := $(KIT)/.build/xcode
# The package does not resolve without the engine's xcframework, so the kit's
# targets build it first when it is missing (`make xcframework` rebuilds it).
XCFRAMEWORK := $(KIT)/Frameworks/DlNzbCore.xcframework

.PHONY: kit-test kit-build kit-build-mac kit-build-ios xcframework mac ios clean

kit-test: $(XCFRAMEWORK)
	cd $(KIT) && swift test

kit-build: kit-build-mac kit-build-ios

kit-build-mac: $(XCFRAMEWORK)
	cd $(KIT) && swift build

# SwiftPM cannot build for iOS on its own; xcodebuild can, through the
# package's generated scheme.
kit-build-ios: $(XCFRAMEWORK)
	cd $(KIT) && xcodebuild -scheme DlNzbKit-Package -destination 'generic/platform=iOS Simulator' \
	  -derivedDataPath $(KIT_DERIVED) build

# The Rust engine as DlNzbKit/Frameworks/DlNzbCore.xcframework (git-ignored),
# plus the regenerated bindings in DlNzbKit/Sources/DlNzbFFI and the crate
# licences in DlNzbUI/Resources/rust-crates.json. Run it before building the
# kit or the apps on a fresh checkout, and after changing the Rust API.
xcframework:
	$(APPLE_DIR)/scripts/build-xcframework.sh

$(XCFRAMEWORK):
	$(APPLE_DIR)/scripts/build-xcframework.sh

# MARK: Mac
#
#   make mac-project   regenerate macos/dl-nzb.xcodeproj from macos/project.yml (needs xcodegen)
#   make mac           build both flavours, Release, ad hoc: build/appstore/dl-nzb.app
#                      (App Store, no Sparkle) and build/direct/dl-nzb.app (Sparkle,
#                      signed by scripts/sign.sh; SIGN_IDENTITY="Developer ID …" for a release)
#   make mac-test      the app's Swift Testing suites
#   make mac-run       build the App Store flavour (Debug) into build/dl-nzb.app and open it
#                      SIMULATE=1   launch with -simulate YES (no server needed)
#                      ARGS="..."   any other launch arguments (-serverHost …)
#   make mac-notarize  notarize and staple build/direct (NOTARY_KEY_ID, NOTARY_ISSUER_ID, NOTARY_KEY_PATH)
#   make mac-dmg       build/direct/dl-nzb-<version>-macOS.dmg and SHA256SUMS
#   make mac-archive   archive and export the App Store flavour for App Store Connect;
#                      needs a team in macos/Config/Local.xcconfig, and never uploads
#
# Builds land in build/ (ignored). The flavours share one bundle identifier,
# so only one should run at a time.
MAC := $(APPLE_DIR)/macos
MAC_DERIVED := $(APPLE_DIR)/build/DerivedData
MAC_RUN_APP := $(APPLE_DIR)/build/dl-nzb.app
SIGN_IDENTITY ?= -

.PHONY: mac-project mac mac-test mac-run mac-notarize mac-dmg mac-archive mac-clean

mac-project:
	DLNZB_VERSION=0.0.0 DLNZB_BUILD=1 xcodegen generate --spec $(MAC)/project.yml --project $(MAC)

mac: mac-project $(XCFRAMEWORK)
	$(APPLE_DIR)/scripts/build-mac.sh appstore
	$(APPLE_DIR)/scripts/build-mac.sh direct
	SIGN_IDENTITY="$(SIGN_IDENTITY)" $(APPLE_DIR)/scripts/sign.sh $(APPLE_DIR)/build/direct/dl-nzb.app

mac-test: mac-project $(XCFRAMEWORK)
	xcodebuild test -project $(MAC)/dl-nzb.xcodeproj -scheme dl-nzb -destination 'platform=macOS' \
	  -derivedDataPath $(MAC_DERIVED)

# A copy that was already running is stopped first: two copies would share one
# queue. It saves its list as it goes, so nothing is lost.
mac-run: mac-project $(XCFRAMEWORK)
	CONFIG=debug OUT=$(APPLE_DIR)/build $(APPLE_DIR)/scripts/build-mac.sh appstore
	pkill -f "$(MAC_RUN_APP)/Contents/MacOS/dl-nzb" || true
	open -n $(MAC_RUN_APP) $(if $(or $(SIMULATE),$(ARGS)),--args $(if $(SIMULATE),-simulate YES) $(ARGS))

mac-notarize:
	$(APPLE_DIR)/scripts/notarize.sh $(APPLE_DIR)/build/direct/dl-nzb.app

mac-dmg:
	SIGN_IDENTITY="$(SIGN_IDENTITY)" $(APPLE_DIR)/scripts/make-dmg.sh $(APPLE_DIR)/build/direct/dl-nzb.app

mac-archive: mac-project $(XCFRAMEWORK)
	$(APPLE_DIR)/scripts/archive-appstore.sh

mac-clean:
	rm -rf $(APPLE_DIR)/build

# MARK: iPhone and iPad
#
#   make ios-project   regenerate ios/dl-nzb.xcodeproj from ios/project.yml (needs xcodegen)
#   make ios           build the app for the iOS Simulator (signed to run locally,
#                      no team needed; the Keychain refuses an unsigned app)
#   make ios-test      the app's Swift Testing suites on a simulator
#   make ios-run       boot the simulator, install and launch the app
#                      SIMULATE=1   launch with -simulate YES (no server needed)
#                      OPEN=<path>  open an NZB, or every NZB in a folder, at
#                                   launch, through the same path as Files
#                      ARGS="..."   any other launch arguments (-serverHost …)
#                      DEVICE=ipad  the iPad simulator instead of the iPhone
#
# The simulators are created on first use ("dl-nzb iPhone", "dl-nzb iPad"),
# see ios/scripts/simulator.sh. Builds land in ios/build (ignored).
IOS := $(APPLE_DIR)/ios
IOS_DERIVED := $(IOS)/build/DerivedData
IOS_CONFIG ?= Debug
IOS_APP := $(IOS_DERIVED)/Build/Products/$(IOS_CONFIG)-iphonesimulator/dl-nzb.app
IOS_BUNDLE_ID := com.zephleggett.dl-nzb

.PHONY: ios-project ios ios-test ios-run

ios-project:
	DLNZB_VERSION=0.0.0 DLNZB_BUILD=1 xcodegen generate --spec $(IOS)/project.yml --project $(IOS)

ios: ios-project
	xcodebuild build -project $(IOS)/dl-nzb.xcodeproj -scheme "dl-nzb Debug" -configuration $(IOS_CONFIG) \
	  -destination 'generic/platform=iOS Simulator' -derivedDataPath $(IOS_DERIVED)

ios-test: ios-project
	udid="$$($(IOS)/scripts/simulator.sh)"; \
	xcodebuild test -project $(IOS)/dl-nzb.xcodeproj -scheme "dl-nzb Debug" \
	  -destination "id=$$udid" -derivedDataPath $(IOS_DERIVED)

ios-run: ios
	udid="$$($(IOS)/scripts/simulator.sh)"; \
	xcrun simctl boot "$$udid" 2>/dev/null || true; \
	open -a Simulator --args -CurrentDeviceUDID "$$udid"; \
	xcrun simctl bootstatus "$$udid" -b >/dev/null; \
	xcrun simctl install "$$udid" "$(IOS_APP)"; \
	xcrun simctl launch --terminate-running-process "$$udid" $(IOS_BUNDLE_ID) \
	  $(if $(SIMULATE),-simulate YES) $(if $(OPEN),-openOnLaunch "$(OPEN)") $(ARGS)

clean:
	rm -rf $(KIT)/.build

# MARK: Releases
#
# What .github/workflows/release.yml and testflight.yml run (docs/releasing.md).
#
#   make mac-direct      the Direct flavour alone, Release, universal, signed by
#                        scripts/sign.sh: build/direct/dl-nzb.app. A release sets
#                        SIGN_IDENTITY, DLNZB_VERSION and DLNZB_UPDATE_FEED_URL
#   make mac-appcast     build/direct/appcast.xml, the update feed for the DMG
#                        (after mac-dmg; SPARKLE_ED_PRIVATE_KEY or SPARKLE_ED_KEY_FILE)
#   make ios-testflight  archive the iPhone app, upload it and publish it on TestFlight
#                        (DLNZB_TEAM, ASC_KEY_ID, ASC_ISSUER_ID, ASC_KEY_PATH;
#                        UPLOAD=0 archives and exports only)
#   make check-version TAG=v0.8.0   the tag against Cargo.toml's version
.PHONY: mac-direct mac-appcast ios-testflight check-version

mac-direct: mac-project $(XCFRAMEWORK)
	$(APPLE_DIR)/scripts/build-mac.sh direct
	SIGN_IDENTITY="$(SIGN_IDENTITY)" $(APPLE_DIR)/scripts/sign.sh $(APPLE_DIR)/build/direct/dl-nzb.app

mac-appcast:
	$(APPLE_DIR)/scripts/make-appcast.sh $(APPLE_DIR)/build/direct/dl-nzb.app

ios-testflight:
	$(IOS)/scripts/testflight.sh

check-version:
	$(APPLE_DIR)/scripts/check-version.sh "$(TAG)"
