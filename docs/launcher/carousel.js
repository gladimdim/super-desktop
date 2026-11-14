/* Native scrolling handles swipes and works without JavaScript. No autoplay. */
(() => {
  const gallery = document.querySelector('.screenshot-carousel');
  if (!gallery) return;
  const track = gallery.querySelector('.gallery-track');
  const slides = [...track.querySelectorAll('.gallery-slide')];
  const picker = [...gallery.querySelectorAll('[data-slide]')];
  const counter = gallery.querySelector('.gallery-count');
  const reduced = matchMedia('(prefers-reduced-motion: reduce)');
  let current = 0;
  let scrollTimer;

  const mark = (index) => {
    current = index;
    picker.forEach((link, i) => {
      if (i === index) link.setAttribute('aria-current', 'true');
      else link.removeAttribute('aria-current');
    });
    const label = `${index + 1} / ${slides.length}`;
    if (counter.textContent !== label) counter.textContent = label;
  };
  const go = (index) => {
    index = (index + slides.length) % slides.length;
    const left = slides[index].getBoundingClientRect().left - track.getBoundingClientRect().left + track.scrollLeft;
    mark(index);
    track.scrollTo({left, behavior: reduced.matches ? 'instant' : 'smooth'});
  };
  const sync = () => {
    const edge = track.getBoundingClientRect().left;
    const distances = slides.map(slide => Math.abs(slide.getBoundingClientRect().left - edge));
    mark(distances.indexOf(Math.min(...distances)));
  };
  gallery.querySelector('.gallery-controls').hidden = false;
  gallery.querySelector('.gallery-hint').textContent = 'Swipe, use the arrows, or choose a feature. Select a screenshot to open it full size.';
  gallery.querySelectorAll('[data-step]').forEach(button => {
    button.addEventListener('click', () => go(current + Number(button.dataset.step)));
  });
  picker.forEach((link, i) => link.addEventListener('click', event => {
    event.preventDefault();
    go(i);
  }));
  track.addEventListener('keydown', event => {
    if (event.altKey || event.ctrlKey || event.metaKey) return;
    const keys = {ArrowLeft: current - 1, ArrowRight: current + 1, Home: 0, End: slides.length - 1};
    if (!(event.key in keys)) return;
    event.preventDefault();
    go(keys[event.key]);
  });
  track.addEventListener('scroll', () => {
    clearTimeout(scrollTimer);
    scrollTimer = setTimeout(sync, 100);
  }, {passive: true});
  // Tabbing to an off-screen image brings its whole slide into view.
  track.addEventListener('focusin', event => {
    const index = slides.indexOf(event.target.closest('.gallery-slide'));
    if (index >= 0 && index !== current) go(index);
  });
})();
