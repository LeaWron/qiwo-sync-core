.PHONY: all install deps release debug setup

all: release
install: install-release

# ── Auto-setup: ensure submodules exist ────────────────────────

LIBRIME_CM = librime/CMakeLists.txt
PLUM_MF = plum/Makefile
SPARKLE_XP = Sparkle/Sparkle.xcodeproj

setup:
	@if [ ! -f $(LIBRIME_CM) ] || [ ! -f $(PLUM_MF) ] || [ ! -d $(SPARKLE_XP) ]; then \
		echo "*** Missing submodules, initializing..."; \
		if [ -f .gitmodules ] || [ -d .git ]; then \
			git submodule update --init --recursive 2>/dev/null || true; \
		fi; \
		[ ! -f $(LIBRIME_CM) ] && git clone --depth 1 https://github.com/rime/librime.git librime && cd librime && git submodule update --init --recursive --depth 1; \
		[ ! -f $(PLUM_MF) ]    && git clone --depth 1 https://github.com/rime/plum.git plum; \
		[ ! -d $(SPARKLE_XP) ] && git clone --depth 1 https://github.com/sparkle-project/Sparkle.git Sparkle; \
	fi

deps: setup

RIME_BIN_DIR = librime/dist/bin
RIME_LIB_DIR = librime/dist/lib
DERIVED_DATA_PATH = build

RIME_LIBRARY_FILE_NAME = librime.1.dylib
RIME_LIBRARY = lib/$(RIME_LIBRARY_FILE_NAME)

RIME_DEPS = librime/lib/libmarisa.a \
	librime/lib/libleveldb.a \
	librime/lib/libopencc.a \
	librime/lib/libyaml-cpp.a
PLUM_DATA = bin/rime-install \
	data/plum/default.yaml \
	data/plum/symbols.yaml \
	data/plum/essay.txt
OPENCC_DATA = data/opencc/TSCharacters.ocd2 \
	data/opencc/TSPhrases.ocd2 \
	data/opencc/t2s.json
SPARKLE_FRAMEWORK = Frameworks/Sparkle.framework
PACKAGE = package/Qiwo.pkg
DEPS_CHECK = $(RIME_LIBRARY) $(PLUM_DATA) $(OPENCC_DATA) $(SPARKLE_FRAMEWORK)

OPENCC_DATA_OUTPUT = librime/share/opencc/*.*
PLUM_DATA_OUTPUT = plum/output/*.*
PLUM_OPENCC_OUTPUT = plum/output/opencc/*.*
RIME_PACKAGE_INSTALLER = plum/rime-install

INSTALL_NAME_TOOL = $(shell xcrun -find install_name_tool)
INSTALL_NAME_TOOL_ARGS = -add_rpath @loader_path/../Frameworks

.PHONY: librime copy-rime-binaries

$(RIME_LIBRARY):
	$(MAKE) librime

$(RIME_DEPS):
	@mkdir -p .bin; \
	printf '#!/bin/sh\nexec /usr/bin/python3 "$$@"\n' > .bin/python; \
	chmod +x .bin/python
	PATH="$$PWD/.bin:$$PATH" $(MAKE) -C librime deps NOPARALLEL=1 -j$$(sysctl -n hw.ncpu 2>/dev/null || echo 4)

librime: $(RIME_DEPS)
	$(MAKE) -C librime release install
	$(MAKE) copy-rime-binaries

copy-rime-binaries:
	cp -L $(RIME_LIB_DIR)/$(RIME_LIBRARY_FILE_NAME) lib/
	cp -pR $(RIME_LIB_DIR)/rime-plugins lib/ 2>/dev/null || true
	cp $(RIME_BIN_DIR)/rime_deployer bin/
	cp $(RIME_BIN_DIR)/rime_dict_manager bin/
	$(INSTALL_NAME_TOOL) $(INSTALL_NAME_TOOL_ARGS) bin/rime_deployer
	$(INSTALL_NAME_TOOL) $(INSTALL_NAME_TOOL_ARGS) bin/rime_dict_manager

.PHONY: data plum-data opencc-data copy-plum-data copy-opencc-data

data: plum-data opencc-data

$(PLUM_DATA):
	$(MAKE) plum-data

$(OPENCC_DATA):
	$(MAKE) opencc-data

plum-data:
	$(MAKE) -C plum
ifdef PLUM_TAG
	rime_dir=plum/output bash plum/rime-install $(PLUM_TAG)
endif
	$(MAKE) copy-plum-data

opencc-data:
	$(MAKE) -C librime deps/opencc
	$(MAKE) copy-opencc-data

copy-plum-data:
	mkdir -p data/plum
	cp $(PLUM_DATA_OUTPUT) data/plum/
	cp $(RIME_PACKAGE_INSTALLER) bin/

copy-opencc-data:
	mkdir -p data/opencc
	cp $(OPENCC_DATA_OUTPUT) data/opencc/
	cp $(PLUM_OPENCC_OUTPUT) data/opencc/ > /dev/null 2>&1 || true

deps: librime data

ifdef ARCHS
BUILD_SETTINGS += ARCHS="$(ARCHS)"
BUILD_SETTINGS += ONLY_ACTIVE_ARCH=NO
_=$() $()
export CMAKE_OSX_ARCHITECTURES = $(subst $(_),;,$(ARCHS))
endif

ifdef MACOSX_DEPLOYMENT_TARGET
BUILD_SETTINGS += MACOSX_DEPLOYMENT_TARGET="$(MACOSX_DEPLOYMENT_TARGET)"
endif

BUILD_SETTINGS += COMPILER_INDEX_STORE_ENABLE=YES

release: $(DEPS_CHECK)
	mkdir -p $(DERIVED_DATA_PATH)
	bash package/add_data_files
	xcodebuild -project Qiwo.xcodeproj -configuration Release -scheme Qiwo -derivedDataPath $(DERIVED_DATA_PATH) $(BUILD_SETTINGS) build
	cp resources/qiwo.pdf $(DERIVED_DATA_PATH)/Build/Products/Release/Qiwo.app/Contents/Resources/
	cp -R qiwo-sync $(DERIVED_DATA_PATH)/Build/Products/Release/Qiwo.app/Contents/Resources/

debug: $(DEPS_CHECK)
	mkdir -p $(DERIVED_DATA_PATH)
	bash package/add_data_files
	xcodebuild -project Qiwo.xcodeproj -configuration Debug -scheme Qiwo -derivedDataPath $(DERIVED_DATA_PATH)  $(BUILD_SETTINGS) build
	cp resources/qiwo.pdf $(DERIVED_DATA_PATH)/Build/Products/Debug/Qiwo.app/Contents/Resources/
	cp -R qiwo-sync $(DERIVED_DATA_PATH)/Build/Products/Debug/Qiwo.app/Contents/Resources/

.PHONY: sparkle copy-sparkle-framework

$(SPARKLE_FRAMEWORK):
	git submodule update --init --recursive Sparkle 2>/dev/null || true
	$(MAKE) sparkle

sparkle:
	xcodebuild -project Sparkle/Sparkle.xcodeproj -configuration Release $(BUILD_SETTINGS) build
	$(MAKE) copy-sparkle-framework

package/generate_keys:
	xcodebuild -project Sparkle/Sparkle.xcodeproj -scheme generate_keys -configuration Release -derivedDataPath Sparkle/build $(BUILD_SETTINGS) build
	cp Sparkle/build/Build/Products/Release/generate_keys package/

package/sign_update:
	xcodebuild -project Sparkle/Sparkle.xcodeproj -scheme sign_update -configuration Release -derivedDataPath Sparkle/build $(BUILD_SETTINGS) build
	cp Sparkle/build/Build/Products/Release/sign_update package/

copy-sparkle-framework:
	mkdir -p Frameworks
	cp -RP Sparkle/build/Release/Sparkle.framework Frameworks/

clean-sparkle:
	rm -rf Frameworks/* > /dev/null 2>&1 || true
	rm -rf Sparkle/build > /dev/null 2>&1 || true

.PHONY: package archive

$(PACKAGE):
ifdef DEV_ID
	bash package/sign_app "$(DEV_ID)" "$(DERIVED_DATA_PATH)"
endif
	bash package/make_package "$(DERIVED_DATA_PATH)"
ifdef DEV_ID
	productsign --sign "Developer ID Installer: $(DEV_ID)" package/Qiwo.pkg package/Qiwo-signed.pkg
	rm package/Qiwo.pkg
	mv package/Qiwo-signed.pkg package/Qiwo.pkg
	xcrun notarytool submit package/Qiwo.pkg --keychain-profile "$(DEV_ID)" --wait
	xcrun stapler staple package/Qiwo.pkg
endif

package: release $(PACKAGE)

archive: package package/sign_update
	bash package/make_archive

DSTROOT = /Library/Input Methods
SQUIRREL_APP_ROOT = $(DSTROOT)/Qiwo.app

.PHONY: permission-check install-debug install-release

permission-check:
	[ -w "$(DSTROOT)" ] || sudo chown ${USER} "$(DSTROOT)" 2>/dev/null || true
	[ ! -d "$(SQUIRREL_APP_ROOT)" ] || [ -w "$(SQUIRREL_APP_ROOT)" ] || sudo chown -R ${USER} "$(SQUIRREL_APP_ROOT)" 2>/dev/null || true

install-debug: debug permission-check
	rm -rf "$(SQUIRREL_APP_ROOT)"
	rm -rf "$(DSTROOT)/Squirrel.app"
	cp -R $(DERIVED_DATA_PATH)/Build/Products/Debug/Qiwo.app "$(DSTROOT)"
	DSTROOT="$(DSTROOT)" RIME_NO_PREBUILD=1 bash scripts/postinstall

install-release: release permission-check
	rm -rf "$(SQUIRREL_APP_ROOT)"
	rm -rf "$(DSTROOT)/Squirrel.app"
	cp -R $(DERIVED_DATA_PATH)/Build/Products/Release/Qiwo.app "$(DSTROOT)"
	DSTROOT="$(DSTROOT)" bash scripts/postinstall

.PHONY: clean clean-deps

clean:
	rm -rf build > /dev/null 2>&1 || true
	rm build.log > /dev/null 2>&1 || true
	rm bin/* > /dev/null 2>&1 || true
	rm lib/* > /dev/null 2>&1 || true
	rm lib/rime-plugins/* > /dev/null 2>&1 || true
	rm data/plum/* > /dev/null 2>&1 || true
	rm data/opencc/* > /dev/null 2>&1 || true

clean-package:
	rm -rf package/*appcast.xml > /dev/null 2>&1 || true
	rm -rf package/*.pkg > /dev/null 2>&1 || true
	rm -rf package/sign_update > /dev/null 2>&1 || true

clean-deps:
	$(MAKE) -C plum clean
	$(MAKE) -C librime clean
	rm -rf librime/dist > /dev/null 2>&1 || true
	$(MAKE) clean-sparkle
