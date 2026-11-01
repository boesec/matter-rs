# matter.js counterparts for the interop harness.
#
# The image carries the pinned @matter/* packages from package-lock.json: the shell
# (`node_modules/@matter/nodejs-shell/dist/cjs/app.js`) is the second controller, `device.mjs` is the first device
# under test. The base image is pinned by digest; the same digest is recorded in
# ../images.toml, which is the list the harness and the README read.
FROM node:22-bookworm-slim@sha256:4b150b48b4116e2a22f2c39cc27025fbf4f153e2078fd3b14d159fdb5c7279da

WORKDIR /app

COPY package.json package-lock.json ./

# --ignore-scripts: the optional Bluetooth packages of the shell would try to compile native
# code, which this image neither needs (IP only) nor has a compiler for.
RUN npm ci --ignore-scripts && npm cache clean --force

COPY device.mjs ./

# Colour-free logs for the collected log files.
ENV MATTER_LOG_FORMAT=plain

CMD ["node", "device.mjs"]
