build:
  cargo zigbuild --release --target aarch64-unknown-linux-musl --features web

copy: build
  scp target/aarch64-unknown-linux-musl/release/paperwave aviar:~/paperwave
