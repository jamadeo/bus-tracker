TARGET := "aarch64-unknown-linux-musl"
HOST := "hassio@homeassistant.local"
REMOTE_DIR := "/addons/bus_tracker/"

default: build

build:
	@echo "🛠️ Building for {{TARGET}} using Zig..."
	cargo zigbuild --release --target {{TARGET}}
	@echo "✅ Build complete."

install: build
	@echo "🚀 Deploying to Home Assistant..."
	rsync -rlvz \
		--filter=":- .gitignore" \
		--exclude='.git' \
		./ \
		{{HOST}}:{{REMOTE_DIR}}
	rsync -rlvz \
	    target/{{TARGET}}/release/bus-tracker \
		{{HOST}}:{{REMOTE_DIR}}
	@echo "\n🎉 Deployment complete!"
	@echo "👉 Next steps: HA UI -> Add-on Store -> Check for Updates -> Click 'Update' -> Click 'Start'"
