FROM node:22-slim
RUN useradd --home-dir /sandbox --create-home sandbox
# Install sayiir under /opt, then symlink to /node_modules so ESM resolution
# finds it (walks up from /sandbox/main.mjs → /sandbox/node_modules → /node_modules)
RUN mkdir -p /opt/sayiir-deps && cd /opt/sayiir-deps && npm install sayiir \
    && ln -s /opt/sayiir-deps/node_modules /node_modules
ENV NODE_PATH=/opt/sayiir-deps/node_modules
WORKDIR /sandbox
USER sandbox
