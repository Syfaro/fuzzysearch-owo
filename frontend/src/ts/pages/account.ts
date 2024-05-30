import emitter from "../socket";

interface AccountEvent {
  account_id: string;
}

interface VerifiedEvent extends AccountEvent {
  verified: boolean;
}

interface LoadingStateEvent extends AccountEvent {
  loading_state: string;
}

interface LoadingProgressEvent extends AccountEvent {
  loaded: number;
  total: number;
}

const NO_FORM_SITES = new Set(["Bluesky", "DeviantArt", "Patreon", "Twitter"]);

function getAccountSection(accountID: string): HTMLElement | null {
  return document.querySelector(`section[data-account-id="${accountID}"]`);
}

emitter.on("account_verified", (payload) => {
  const event = payload as VerifiedEvent;

  if (!getAccountSection(event.account_id)) return;

  if (event.verified) {
    alert("Account verified!");
  } else {
    alert("Account could not be verified, please try again.");
  }

  window.location.reload();
});

emitter.on("loading_state_change", (payload) => {
  const event = payload as LoadingStateEvent;

  const loadingStateElement = getAccountSection(
    event.account_id
  )?.querySelector("#account-loading-state");
  if (!loadingStateElement) return;

  if (event.loading_state === "Loading Complete") {
    window.location.reload();
  } else {
    loadingStateElement.textContent = event.loading_state;
  }
});

emitter.on("loading_progress", (payload) => {
  const event = payload as LoadingProgressEvent;

  const progressElement = <HTMLProgressElement | null>(
    getAccountSection(event.account_id)?.querySelector(
      "#account-import-progress"
    )
  );
  if (!progressElement) return;

  progressElement.value = event.loaded;
  progressElement.max = event.total;
});

const siteSelectInput = <HTMLSelectElement | null>(
  document.getElementById("site-select")
);
const usernameInput = <HTMLInputElement | null>(
  document.getElementById("username-input")
);

if (siteSelectInput && usernameInput) {
  const updateSelection = (value: string) => {
    usernameInput.style.display = NO_FORM_SITES.has(value) ? "none" : "block";
  };

  updateSelection(siteSelectInput.value);
  siteSelectInput.addEventListener("change", () => {
    updateSelection(siteSelectInput.value);
  });
}

const verifyAccountButton = document.getElementById("verify-account");
if (verifyAccountButton) {
  const accountID = document.querySelector<HTMLElement>(
    "section[data-account-id]"
  )?.dataset.accountId;
  if (accountID) {
    verifyAccountButton.addEventListener("click", (ev) => {
      verifyAccountButton.classList.add("is-loading");

      fetch("/user/account/verify", {
        method: "POST",
        body: new URLSearchParams({
          account_id: accountID,
        }),
        credentials: "same-origin",
        headers: {
          "content-type": "application/x-www-form-urlencoded",
        },
      });
    });
  }
}

document.getElementById("delete-account")?.addEventListener("click", (ev) => {
  const wasIntentional = confirm(
    "Are you sure you want to remove this account?"
  );
  if (!wasIntentional) {
    ev.preventDefault();
  }
});
