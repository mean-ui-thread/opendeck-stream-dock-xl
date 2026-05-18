
id := "com.github.mean-ui-thread.opendeck-stream-dock-xl.sdPlugin"
os-id := `grep -E '^ID=' /etc/os-release | cut -d= -f2 | tr -d '"'`
uid := `id -u`
gid := `id -g`

release: bump package tag

package: build-linux build-mac build-win collect zip

install-dev-deps:
    #!/usr/bin/env sh
    cargo install cargo-edit
    cargo install git-cliff

    case {{os-id}} in
        ubuntu|debian)
            sudo apt-get install -y mingw-w64
            ;;
        arch|cachyos)
            sudo pacman -S --noconfirm mingw-w64-gcc
            ;;
        *)
            echo "Unsupported distribution: {{os-id}}"
            exit 1
            ;;
    esac

bump next=`git cliff --bumped-version | tr -d "v"`: install-dev-deps
    git diff --cached --exit-code

    echo "We will bump version to {{next}}, press any key"
    read ans

    sed -i 's/"Version": ".*"/"Version": "{{next}}"/g' manifest.json
    sed -i 's/^version = ".*"$/version = "{{next}}"/g' Cargo.toml

tag next=`git cliff --bumped-version`: install-dev-deps
    echo "Generating changelog"
    git cliff -o CHANGELOG.md --tag {{next}}

    echo "We will now commit the changes, please review before pressing any key"
    read ans

    git add .
    git commit -m "chore(release): {{next}}"
    git tag "{{next}}"

build-linux: install-dev-deps
    cargo build --release --target x86_64-unknown-linux-gnu --target-dir target/plugin-linux

build-mac: install-dev-deps
    docker run --rm -v $(pwd):/io -w /io --user {{uid}}:{{gid}} -e HOME=/tmp -e XDG_CACHE_HOME=/tmp/.cache ghcr.io/rust-cross/cargo-zigbuild:0.22.3 cargo zigbuild --release --target universal2-apple-darwin --target-dir target/plugin-mac

build-win: install-dev-deps
    cargo build --release --target x86_64-pc-windows-gnu --target-dir target/plugin-win

clean:
    rm -rf target/

collect:
    rm -rf build
    mkdir -p build/{{id}}
    cp -r assets build/{{id}}
    cp manifest.json build/{{id}}
    cp target/plugin-linux/x86_64-unknown-linux-gnu/release/opendeck-stream-dock-xl build/{{id}}/opendeck-stream-dock-xl-linux
    cp target/plugin-mac/universal2-apple-darwin/release/opendeck-stream-dock-xl build/{{id}}/opendeck-stream-dock-xl-mac
    cp target/plugin-win/x86_64-pc-windows-gnu/release/opendeck-stream-dock-xl.exe build/{{id}}/opendeck-stream-dock-xl-win.exe

[working-directory: "build"]
zip:
    zip -r opendeck-stream-dock-xl.plugin.zip {{id}}/
