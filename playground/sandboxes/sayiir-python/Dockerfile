FROM python:3.13-slim
RUN apt-get update && apt-get install -y --no-install-recommends curl build-essential && \
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && \
    apt-get clean && rm -rf /var/lib/apt/lists/*
ENV PATH="/root/.cargo/bin:${PATH}"
RUN pip install sayiir && \
    rustup self uninstall -y && \
    apt-get purge -y --auto-remove curl build-essential
RUN useradd --home-dir /sandbox --create-home sandbox
USER sandbox
WORKDIR /sandbox
