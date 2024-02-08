document.getElementById("inject-job")?.addEventListener("click", (ev) => {
  if (!confirm("Are you sure you want to inject this job?")) {
    ev.preventDefault();
  }
});
