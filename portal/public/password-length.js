// Checks a server-rendered Portal form's new password against the minimum in
// its data-password-minimum. Auth counts characters as Unicode code points,
// which is what spreading a string counts; HTML minlength counts UTF-16 code
// units and so would let a too-short password with emoji through.
(() => {
  const form = document.querySelector("form[data-password-minimum]");
  const input = form && form.querySelector("#password");
  if (!input) return;
  const minimum = Number(form.dataset.passwordMinimum);
  const clear = () => {
    document.getElementById("password-error")?.remove();
    input.removeAttribute("aria-invalid");
    input.setAttribute("aria-describedby", "password-hint");
  };
  input.addEventListener("input", clear);
  form.addEventListener("submit", (event) => {
    if ([...input.value].length >= minimum) return;
    event.preventDefault();
    clear();
    const error = document.createElement("p");
    error.className = "alert error";
    error.id = "password-error";
    error.setAttribute("role", "alert");
    error.textContent = `Use at least ${minimum} characters.`;
    input.after(error);
    input.setAttribute("aria-invalid", "true");
    input.setAttribute("aria-describedby", "password-error");
    input.focus();
  });
})();
