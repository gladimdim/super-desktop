# Clean Omarchy stable builder for x86_64 and aarch64.
# Bootstrapped directly from Omarchy's stable mirrors so rolling Arch Hub
# images cannot hit core package downgrade conflicts (e.g. openssl vs openssl-3).

FROM docker.io/alpine:3.21 AS bootstrapper

ARG TARGETARCH

COPY pacstrap-docker /usr/local/bin/

RUN apk add --no-cache \
    pacman-makepkg \
    curl \
    bash \
    zstd \
    gnupg

RUN mkdir -p /etc/pacman.d && \
    case "${TARGETARCH}" in \
    amd64) \
        REPOS="[core]\nInclude = /etc/pacman.d/mirrorlist\n\n[extra]\nInclude = /etc/pacman.d/mirrorlist\n" && \
        printf 'Server = https://stable-mirror.omarchy.org/$repo/os/$arch\n' > /etc/pacman.d/mirrorlist ;; \
    arm64) \
        REPOS="[core]\nInclude = /etc/pacman.d/mirrorlist\n\n[extra]\nInclude = /etc/pacman.d/mirrorlist\n\n[alarm]\nInclude = /etc/pacman.d/mirrorlist\n\n[aur]\nInclude = /etc/pacman.d/mirrorlist\n" && \
        printf 'Server = https://arm-stable-mirror.omarchy.org/$arch/$repo\n' > /etc/pacman.d/mirrorlist ;; \
    *) \
        echo "Unsupported TARGETARCH: ${TARGETARCH}" && exit 1 ;; \
    esac && \
    printf '[options]\nHoldPkg = pacman glibc\nArchitecture = auto\nSigLevel = Required DatabaseOptional\nLocalFileSigLevel = Optional\n\n%b' "$REPOS" > /etc/pacman.conf

RUN mkdir -p /usr/share/pacman/keyrings && \
    if [ "${TARGETARCH}" = "arm64" ]; then \
        KEYRING_URL="https://raw.githubusercontent.com/archlinuxarm/PKGBUILDs/master/core/archlinuxarm-keyring/" && \
        for file in archlinuxarm-revoked archlinuxarm-trusted archlinuxarm.gpg; do \
            curl -fsSL "${KEYRING_URL}${file}" -o "/usr/share/pacman/keyrings/${file}" || exit 1; \
        done; \
    else \
        mkdir /tmp/archlinux-keyring && \
        curl -fsSL https://archlinux.org/packages/core/any/archlinux-keyring/download | \
            unzstd | tar -C /tmp/archlinux-keyring -xv && \
        mv /tmp/archlinux-keyring/usr/share/pacman/keyrings/* /usr/share/pacman/keyrings/ && \
        rm -rf /tmp/archlinux-keyring; \
    fi && \
    pacman-key --init && \
    pacman-key --populate

RUN mkdir /rootfs && \
    if [ "${TARGETARCH}" = "arm64" ]; then \
        BOOTSTRAP_EXTRA="archlinuxarm-keyring"; \
    else \
        BOOTSTRAP_EXTRA=""; \
    fi && \
    pacstrap-docker /rootfs base ${BOOTSTRAP_EXTRA} && \
    cp /etc/pacman.conf /rootfs/etc/pacman.conf && \
    cp /etc/pacman.d/mirrorlist /rootfs/etc/pacman.d/mirrorlist && \
    echo "en_US.UTF-8 UTF-8" > /rootfs/etc/locale.gen && \
    echo "LANG=en_US.UTF-8" > /rootfs/etc/locale.conf && \
    rm -rf /rootfs/var/lib/pacman/sync/* \
           /rootfs/var/cache/pacman/pkg/* \
           /rootfs/tmp/*

FROM scratch AS builder

COPY --from=bootstrapper /rootfs/ /

ENV LANG=en_US.UTF-8
ENV PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

RUN ln -sf /usr/lib/os-release /etc/os-release && \
    locale-gen && \
    pacman-key --init && \
    if pacman -Q archlinuxarm-keyring >/dev/null 2>&1; then \
        pacman-key --populate archlinux archlinuxarm && \
        pacman-key --lsign-key 77193F152BDBE6A6; \
    else \
        pacman-key --populate archlinux; \
    fi

RUN pacman -Syu --noconfirm && \
    pacman -S --noconfirm --needed \
        base-devel \
        git \
        rust \
        python \
        gtk4 \
        gtk4-layer-shell \
        vte4 \
        tmux \
        wl-clipboard \
        libnotify \
        sqlite \
        avahi \
        bubblewrap \
        poppler \
        qt6-base \
        qt6-declarative \
        gst-plugins-good \
        gst-libav && \
    pacman -Scc --noconfirm && \
    rm -rf /var/cache/pacman/pkg/*

RUN sed -i 's/^#MAKEFLAGS=.*/MAKEFLAGS="-j$(nproc)"/' /etc/makepkg.conf && \
    sed -i 's/^#PKGEXT=.*/PKGEXT=".pkg.tar.zst"/' /etc/makepkg.conf && \
    sed -i "s|^PKGEXT=.*|PKGEXT='.pkg.tar.zst'|" /etc/makepkg.conf

RUN useradd -m builder
