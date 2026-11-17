// Checks a server-rendered Portal form's new password against the minimum in
// its data-password-minimum, and against Auth's 4096-byte limit. Auth counts
// characters as Unicode code points, which is what spreading a string counts;
// HTML minlength and maxlength count UTF-16 code units, so they would let a
// too-short password with emoji, or a too-long one in other scripts, through.
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
    const problem =
      new TextEncoder().encode(input.value).length > 4096
        ? "That password is too long."
        : [...input.value].length < minimum
          ? `Use at least ${minimum} characters.`
          : null;
    if (!problem) return;
    event.preventDefault();
    clear();
    const error = document.createElement("p");
    error.className = "alert error";
    error.id = "password-error";
    error.setAttribute("role", "alert");
    error.textContent = problem;
    input.after(error);
    input.setAttribute("aria-invalid", "true");
    input.setAttribute("aria-describedby", "password-error");
    input.focus();
  });
})();
