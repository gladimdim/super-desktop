# Fresh Omarchy stable container image for x86_64 and aarch64.
# Bootstraps a clean Omarchy desktop baseline without SUPER DESKTOP or its
# extra runtime/build dependencies so release verification can exercise every
# installer script from scratch.
#
# Build context must be packaging/arch:
#   podman build -f packaging/arch/omarchy-fresh/Dockerfile -t sd-omarchy-fresh:x86_64 packaging/arch

FROM docker.io/alpine:3.21 AS bootstrapper

ARG TARGETARCH

COPY builder/pacstrap-docker /usr/local/bin/pacstrap-docker

RUN chmod +x /usr/local/bin/pacstrap-docker && \
    apk add --no-cache \
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
        REPOS="[core]\nInclude = /etc/pacman.d/mirrorlist\n\n[extra]\nInclude = /etc/pacman.d/mirrorlist\n\n[alarm]\nInclude = /etc/pacman.d/mirrorlist\n" && \
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
            unzstd | tar -C /tmp/archlinux-keyring -xf - && \
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
    rm -rf /rootfs/var/cache/pacman/pkg/* \
           /rootfs/tmp/*

FROM scratch AS omarchy-fresh

ARG TARGETARCH

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

RUN case "${TARGETARCH}" in \
    amd64) OMARCHY_ARCH=x86_64 ;; \
    arm64) OMARCHY_ARCH=aarch64 ;; \
    *) echo "Unsupported TARGETARCH: ${TARGETARCH}" && exit 1 ;; \
    esac && \
    mkdir -p /tmp/omarchy-kr && \
    KEYRING_PKG="$(curl -fsSL "https://pkgs.omarchy.org/stable/${OMARCHY_ARCH}/omarchy.db" | bsdtar -xOf - 'omarchy-keyring-*/desc' | awk '/^%FILENAME%$/ { getline; print; exit }')" && \
    test -n "$KEYRING_PKG" && \
    curl -fsSL "https://pkgs.omarchy.org/stable/${OMARCHY_ARCH}/${KEYRING_PKG}" -o /tmp/omarchy-kr/pkg.tar.zst && \
    bsdtar -C /tmp/omarchy-kr -xf /tmp/omarchy-kr/pkg.tar.zst && \
    cp -a /tmp/omarchy-kr/usr/share/pacman/keyrings/omarchy* /usr/share/pacman/keyrings/ && \
    FPR="$(gpg --batch --show-keys --with-colons /usr/share/pacman/keyrings/omarchy.gpg | awk -F: '$1 == "pub" { in_pub = 1; next } $1 == "sub" { in_pub = 0 } in_pub && $1 == "fpr" { print $10; in_pub = 0 }' | grep -Fx '40DFB630FF42BCFFB047046CF0134EE680CAC571')" && \
    test "$FPR" = "40DFB630FF42BCFFB047046CF0134EE680CAC571" && \
    pacman-key --populate omarchy && \
    rm -f /usr/share/pacman/keyrings/omarchy* && \
    rm -rf /tmp/omarchy-kr

RUN case "${TARGETARCH}" in \
    amd64) \
        printf 'Server = https://stable-mirror.omarchy.org/$repo/os/$arch\n' > /etc/pacman.d/mirrorlist && \
        REPOS="[omarchy]\nServer = https://pkgs.omarchy.org/stable/\$arch\n\n[core]\nInclude = /etc/pacman.d/mirrorlist\n\n[extra]\nInclude = /etc/pacman.d/mirrorlist\n\n[multilib]\nInclude = /etc/pacman.d/mirrorlist\n" ;; \
    arm64) \
        printf 'Server = https://arm-stable-mirror.omarchy.org/$arch/$repo\n' > /etc/pacman.d/mirrorlist && \
        REPOS="[omarchy]\nServer = https://pkgs.omarchy.org/stable/\$arch\n\n[core]\nInclude = /etc/pacman.d/mirrorlist\n\n[extra]\nInclude = /etc/pacman.d/mirrorlist\n\n[alarm]\nInclude = /etc/pacman.d/mirrorlist\n" ;; \
    *) \
        echo "Unsupported TARGETARCH: ${TARGETARCH}" && exit 1 ;; \
    esac && \
    printf '[options]\nColor\nVerbosePkgLists\nHoldPkg = pacman glibc\nArchitecture = auto\nCheckSpace\nParallelDownloads = 5\nDisableSandbox\nSigLevel = Required DatabaseOptional\nLocalFileSigLevel = Optional\n\n%b' "$REPOS" > /etc/pacman.conf

RUN pacman -Syu --noconfirm && \
    pacman -S --noconfirm --needed omarchy && \
    rm -rf /var/cache/pacman/pkg/*

RUN useradd -m -G wheel -s /bin/bash omarchy && \
    printf '%%wheel ALL=(ALL:ALL) NOPASSWD: ALL\n' > /etc/sudoers.d/wheel && \
    chmod 0440 /etc/sudoers.d/wheel && \
    test -f /home/omarchy/.config/hypr/hyprland.lua && \
    test -f /home/omarchy/.config/hypr/bindings.lua && \
    test -f /home/omarchy/.config/hypr/autostart.lua && \
    test -x /usr/bin/omarchy && \
    test -d /usr/share/omarchy && \
    ! pacman -Q super-desktop-bin >/dev/null 2>&1
