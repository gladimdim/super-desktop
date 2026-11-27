/* Production UI behaviors: self-hosted QR modal + multi-instance accessible carousels. */
(function () {
  "use strict";

  const QR_MAP = {
    apk: {
      img: "assets/qr-apk.svg",
      title: "Free Android APK (2.3.1)",
      sub: "Every feature free forever. No trial, no Google Play Billing, SHA-256 verified.",
      url: "https://github.com/gladimdim/super-desktop/releases/download/v2.1.2/SUPER-DESKTOP-2.3.1.apk",
      sha: "https://github.com/gladimdim/super-desktop/releases/download/v2.1.2/SUPER-DESKTOP-2.3.1.apk.sha256",
    },
    play: {
      img: "assets/qr-play.svg",
      title: "Google Play Store",
      sub: "Auto-updates via Play. 0 accounts, 0 analytics. 10-day trial + optional one-time Pro.",
      url: "https://play.google.com/store/apps/details?id=com.gladimdim.superdesktop",
    },
    ios: {
      img: "assets/qr-ios.svg?v=2",
      title: "iPhone & iPad (App Store)",
      sub: "SUPER DESKTOP: AI Agents. Keychain-pinned (no iCloud sync), local alerts.",
      url: "https://apps.apple.com/app/super-desktop-ai-agents/id6818722591",
    },
    launcher: {
      img: "assets/qr-launcher.svg",
      title: "SD Launcher APK (1.15.0)",
      sub: "Open-source Omalauncher fork with built-in SUPER DESKTOP pages & terminal widgets.",
      url: "https://github.com/gladimdim/omalauncher/releases/download/sd-v1.15.0/SD-Launcher-1.15.0.apk",
      sha: "https://github.com/gladimdim/omalauncher/releases/download/sd-v1.15.0/SD-Launcher-1.15.0.apk.sha256",
    },
  };

  function injectDialogStyles() {
    if (document.getElementById("sd-qr-styles")) return;
    const style = document.createElement("style");
    style.id = "sd-qr-styles";
    style.textContent = `
      dialog.sd-qr-dialog {
        margin: auto; border: 2px solid var(--line-hi, #a3e635); padding: 22px;
        background: var(--pane, #0b0b0b); color: var(--text, #f3f4f6);
        max-width: 390px; width: calc(100vw - 32px);
        box-shadow: 0 28px 80px rgba(0, 0, 0, 0.65);
        font-family: var(--mono, ui-monospace, monospace);
      }
      dialog.sd-qr-dialog::backdrop { background: rgba(4, 6, 10, 0.72); backdrop-filter: blur(4px); }
      .sd-qr-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 12px; margin-bottom: 10px; }
      .sd-qr-head h3 { margin: 0; font-size: 16px; line-height: 1.3; color: var(--lime, #a3e635); }
      .sd-qr-close {
        border: 2px solid var(--line-hi, #e5e5e5); background: var(--bar-bg, #000); color: var(--text, #f3f4f6);
        width: 32px; height: 32px; font-size: 15px; font-weight: 700; cursor: pointer; flex: none;
      }
      .sd-qr-close:hover { background: var(--lime, #a3e635); border-color: var(--lime, #a3e635); color: var(--lime-fg, #000); }
      .sd-qr-sub { margin: 0 0 14px; font-size: 12.5px; color: var(--muted, #9ca3af); line-height: 1.45; }
      .sd-qr-frame {
        background: #ffffff; border: 2px solid var(--line, #2c2c2c); padding: 14px;
        display: grid; place-items: center; margin-bottom: 14px;
      }
      .sd-qr-frame img { width: 210px; height: 210px; display: block; }
      .sd-qr-actions { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; justify-content: space-between; font-size: 12px; }
      .sd-qr-actions a {
        color: var(--lime, #a3e635); font-weight: 700; text-decoration: none;
        padding: 8px 11px; border: 1px solid var(--lime, #a3e635); background: var(--bar-bg, #000);
      }
      .sd-qr-actions a:hover { background: var(--lime, #a3e635); color: var(--lime-fg, #000); text-decoration: none; }
    `;
    document.head.appendChild(style);
  }

  function setupQrModal() {
    const triggers = document.querySelectorAll("[data-qr]");
    if (!triggers.length) return;
    injectDialogStyles();

    const dialog = document.createElement("dialog");
    dialog.className = "sd-qr-dialog";
    dialog.innerHTML = `
      <div class="sd-qr-head">
        <h3 id="sd-qr-title"></h3>
        <button type="button" class="sd-qr-close" aria-label="Close QR dialog">✕</button>
      </div>
      <p class="sd-qr-sub" id="sd-qr-sub"></p>
      <div class="sd-qr-frame"><img id="sd-qr-img" width="210" height="210" alt="QR code"></div>
      <div class="sd-qr-actions">
        <a id="sd-qr-link" href="#" target="_blank" rel="noopener">OPEN LINK →</a>
        <a id="sd-qr-sha" href="#" target="_blank" rel="noopener" hidden>SHA-256</a>
      </div>
    `;
    document.body.appendChild(dialog);

    const closeBtn = dialog.querySelector(".sd-qr-close");
    closeBtn.addEventListener("click", () => dialog.close());
    dialog.addEventListener("click", (e) => {
      if (e.target === dialog) dialog.close();
    });

    triggers.forEach((btn) => {
      btn.addEventListener("click", () => {
        const info = QR_MAP[btn.dataset.qr];
        if (!info) return;
        dialog.querySelector("#sd-qr-title").textContent = btn.dataset.qrTitle || info.title;
        dialog.querySelector("#sd-qr-sub").textContent = btn.dataset.qrSub || info.sub;
        const img = dialog.querySelector("#sd-qr-img");
        img.src = info.img;
        img.alt = `QR code for ${info.title}`;
        dialog.querySelector("#sd-qr-link").href = btn.dataset.qrUrl || info.url;
        const shaLink = dialog.querySelector("#sd-qr-sha");
        if (info.sha) {
          shaLink.href = info.sha;
          shaLink.hidden = false;
        } else {
          shaLink.hidden = true;
        }
        if (typeof dialog.showModal === "function") dialog.showModal();
        else dialog.setAttribute("open", "");
      });
    });
  }

  function setupCarousels() {
    document.querySelectorAll("[data-carousel]").forEach((root) => {
      const slides = Array.from(root.querySelectorAll(".car-slide"));
      if (!slides.length) return;
      const pills = Array.from(root.querySelectorAll("[data-car-goto]"));
      const countEl = root.querySelector("[data-car-count]");
      const prevBtn = root.querySelector("[data-car-prev]");
      const nextBtn = root.querySelector("[data-car-next]");
      const stage = root.querySelector(".car-stage") || root;
      let idx = 0;

      const pad2 = (n) => String(n).padStart(2, "0");
      const show = (next) => {
        idx = (next + slides.length) % slides.length;
        slides.forEach((s, i) => { s.hidden = i !== idx; });
        pills.forEach((b, i) => {
          if (i === idx) b.setAttribute("aria-current", "true");
          else b.removeAttribute("aria-current");
        });
        if (countEl) countEl.textContent = `${pad2(idx + 1)} / ${pad2(slides.length)}`;
      };

      if (prevBtn) prevBtn.addEventListener("click", () => show(idx - 1));
      if (nextBtn) nextBtn.addEventListener("click", () => show(idx + 1));
      pills.forEach((b) => {
        b.addEventListener("click", () => show(Number(b.dataset.carGoto || 0)));
      });

      root.addEventListener("keydown", (e) => {
        if (e.altKey || e.ctrlKey || e.metaKey) return;
        if (e.key === "ArrowLeft") { e.preventDefault(); show(idx - 1); }
        else if (e.key === "ArrowRight") { e.preventDefault(); show(idx + 1); }
      });

      let touchX = null;
      stage.addEventListener("touchstart", (e) => {
        if (e.touches && e.touches.length === 1) touchX = e.touches[0].clientX;
      }, { passive: true });
      stage.addEventListener("touchend", (e) => {
        if (touchX === null || !e.changedTouches || !e.changedTouches.length) return;
        const dx = e.changedTouches[0].clientX - touchX;
        touchX = null;
        if (Math.abs(dx) > 40) show(dx < 0 ? idx + 1 : idx - 1);
      }, { passive: true });

      show(0);
    });
  }

  function init() {
    setupQrModal();
    setupCarousels();
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();
