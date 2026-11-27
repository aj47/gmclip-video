const form = document.querySelector('.signup');
const status = document.querySelector('#signup-status');
const submit = form.querySelector('button');
const submitLabel = submit.querySelector('span');
const dialog = document.querySelector('#signup-dialog');
const intro = dialog.querySelector('.signup-intro');
const success = dialog.querySelector('.signup-success');
let opener;

document.querySelectorAll('[data-signup-open]').forEach((button) => {
  button.addEventListener('click', () => {
    opener = button;
    dialog.showModal();
    document.documentElement.classList.add('signup-open');
  });
});
dialog.querySelectorAll('[data-signup-close]').forEach((button) => {
  button.addEventListener('click', () => dialog.close());
});
// The native dialog handles Escape, focus trapping, and inert background content.
dialog.addEventListener('keydown', (event) => {
  if (event.key !== 'Tab') return;
  const controls = [...dialog.querySelectorAll('button, input, [tabindex]')]
    .filter((control) => !control.disabled && control.tabIndex >= 0 && control.getClientRects().length);
  const first = controls[0];
  const last = controls.at(-1);
  if (event.shiftKey && (document.activeElement === first || document.activeElement === success)) {
    event.preventDefault();
    last?.focus();
  } else if (!event.shiftKey && document.activeElement === last) {
    event.preventDefault();
    first?.focus();
  }
});
dialog.addEventListener('click', (event) => {
  const rect = dialog.getBoundingClientRect();
  if (event.target === dialog && (event.clientX < rect.left || event.clientX > rect.right || event.clientY < rect.top || event.clientY > rect.bottom)) dialog.close();
});
dialog.addEventListener('close', () => {
  document.documentElement.classList.remove('signup-open');
  opener?.focus({ preventScroll: true });
});

form.addEventListener('submit', async (event) => {
  event.preventDefault();
  if (!form.reportValidity() || submit.disabled) return;
  submit.disabled = true;
  submitLabel.textContent = 'Joining…';
  status.textContent = '';
  status.removeAttribute('data-error');
  try {
    const response = await fetch(form.action, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ email: form.elements.email.value, website: form.elements.website.value }),
      signal: AbortSignal.timeout(15000),
    });
    const result = await response.json();
    if (!response.ok) throw new Error(result.error || 'We couldn’t save your email. Please try again.');
    intro.hidden = true;
    form.hidden = true;
    success.hidden = false;
    window.gmCount?.('signup');
    if (dialog.open) success.focus();
    form.reset();
  } catch (error) {
    status.dataset.error = 'true';
    status.textContent = error.name === 'TimeoutError' || error.name === 'TypeError'
      ? 'Couldn’t connect. Check your connection and try again.'
      : error.message;
  } finally {
    submit.disabled = false;
    submitLabel.textContent = 'Get updates';
  }
});
