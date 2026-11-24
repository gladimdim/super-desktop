// Interactive preview switcher for the Telegram Usage and Setup screens.
(() => {
  const tabButtons = Array.from(document.querySelectorAll('.screen-tabs [role="tab"]'));
  const panels = new Map(
    tabButtons.map((btn) => [btn, document.getElementById(btn.getAttribute('aria-controls'))])
  );

  function selectScreen(activeBtn) {
    for (const [btn, panel] of panels.entries()) {
      const active = btn === activeBtn;
      btn.setAttribute('aria-selected', active ? 'true' : 'false');
      if (panel) panel.hidden = !active;
    }
  }

  tabButtons.forEach((btn) => {
    btn.addEventListener('click', () => selectScreen(btn));
  });

  const topicButtons = Array.from(document.querySelectorAll('.tg-topic-btn'));
  const topicPanes = new Map(
    topicButtons.map((btn) => [btn, document.getElementById(btn.dataset.topicTarget)])
  );

  function selectTopic(activeBtn) {
    for (const [btn, pane] of topicPanes.entries()) {
      const active = btn === activeBtn;
      btn.setAttribute('aria-selected', active ? 'true' : 'false');
      if (pane) pane.hidden = !active;
    }
  }

  topicButtons.forEach((btn) => {
    btn.addEventListener('click', () => selectTopic(btn));
  });

  const copyPromptBtn = document.getElementById('copy-agent-prompt');
  const promptTextEl = document.getElementById('agent-setup-prompt');
  if (copyPromptBtn && promptTextEl) {
    copyPromptBtn.addEventListener('click', async () => {
      try {
        await navigator.clipboard.writeText(promptTextEl.textContent.trim());
        const prev = copyPromptBtn.textContent;
        copyPromptBtn.textContent = '✓ Copied!';
        setTimeout(() => { copyPromptBtn.textContent = prev; }, 2000);
      } catch (_) {}
    });
  }
})();
