download-trunk:
    #!/usr/bin/env bash
    if [ ! -f ./trunk ]; then
        @echo "Downloading trunk..."
        wget -qO- https://github.com/trunk-rs/trunk/releases/download/v0.20.2/trunk-aarch64-apple-darwin.tar.gz | tar -xzf-
    fi

build-pages: download-trunk
    @echo "Building pages..."
    @CC=/opt/homebrew/opt/llvm/bin/clang AR=/opt/homebrew/opt/llvm/bin/llvm-ar RUSTFLAGS="--cfg=web_sys_unstable_apis" ./trunk build surfer/index.html --release --public-url /dist --features accesskit
    sed -i '' 's/\/dist\//.\//g' surfer/dist/index.html
    @echo "Building pages done."

deploy-pages branch="main": build-pages
    @wrangler pages deploy surfer/dist/ --branch "$branch"
