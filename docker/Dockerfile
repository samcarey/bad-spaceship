FROM ubuntu:20.04

RUN apt-get update && \
    apt-get install -y \
    curl \
    git \
    htop \
    vim \
    build-essential \
    lld \
    clang \
    pkg-config \
    libasound2-dev \
    xorg \
    openbox \
    libx11-dev
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y

ENV PATH="/root/.cargo/bin:${PATH}"

RUN rustup toolchain install nightly && \
    rustup default nightly

RUN mkdir -p projects && \
    cd projects && \
    git clone https://github.com/bevyengine/bevy.git
