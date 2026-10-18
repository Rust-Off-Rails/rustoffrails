// The planted mistake of the lab widget page: it binds its button when DOMContentLoaded fires.
// A boosted link swaps the body and runs this long after that event, so the button stays dead.
document.addEventListener("DOMContentLoaded", function () {
  var button = document.getElementById("show-code");
  var code = document.getElementById("lab-code");
  if (!button || !code) return;
  button.addEventListener("click", function () {
    code.hidden = !code.hidden;
    button.textContent = code.hidden ? "Show" : "Hide";
  });
});
