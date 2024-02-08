/**
 * Bluesky-specific logic.
 *
 * Currently responsible for starting a DID resolution.
 */

import emitter from "../socket";

interface BlueskyDidEvent {
  did: string;
  result: BlueskyDidResult;
}

interface BlueskyDidResult {
  status: "success" | "error";
  message: string | null;
  service_endpoint: string | null;
}

const usernameInput = <HTMLInputElement | null>(
  document.getElementById("bluesky-username")
);
const serverInput = <HTMLInputElement | null>(
  document.getElementById("bluesky-server")
);
const submitButton = <HTMLButtonElement | null>(
  document.getElementById("bluesky-verify")
);
const helpText = document.querySelector("#bluesky-field .help");

document.getElementById("bluesky-username")?.addEventListener("blur", (ev) => {
  const username = (ev.target as HTMLInputElement).value.trim();
  if (username.length === 0) {
    return;
  }

  if (helpText) {
    helpText.textContent = "Resolving your username…";
  }

  submitButton?.classList.add("is-loading");

  const url = new URL("/bluesky/resolve-did", window.location.href);
  url.searchParams.set("did", username);

  fetch(url);
});

emitter.on("resolved_did", (payload) => {
  const event = payload as BlueskyDidEvent;

  if (usernameInput?.value != event.did) return;

  const isSuccess = event.result.status === "success";
  if (!isSuccess) {
    console.error("Could not resolve DID", event.result.message);
  }

  if (submitButton) {
    submitButton.disabled = !isSuccess;
    submitButton.classList.remove("is-loading");
  }

  if (isSuccess && serverInput) {
    serverInput.value = event.result.service_endpoint || "";
  }

  usernameInput.classList.toggle("is-success", isSuccess);
  usernameInput.classList.toggle("is-danger", !isSuccess);

  if (helpText) {
    helpText.classList.toggle("is-hidden", isSuccess);

    if (!isSuccess) {
      helpText.textContent = "Sorry, your username could not be resolved.";
    }
  }
});
