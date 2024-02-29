import {
  browserSupportsWebAuthnAutofill,
  startAuthentication,
  startRegistration,
} from "@simplewebauthn/browser";
import {
  AuthenticationResponseJSON,
  PublicKeyCredentialCreationOptionsJSON,
  RegistrationResponseJSON,
} from "@simplewebauthn/types";

document.getElementById("passkey-add")?.addEventListener(
  "click",
  async (ev) => {
    ev.preventDefault();

    try {
      await performRegistration();
    } catch (error) {
      alert(`Could not perform Passkey registration: ${error}`);
      return;
    }
  },
);

if (window.location.pathname === "/auth/login") {
  performPasswordlessLogin();
}

async function performPasswordlessLogin() {
  if (!(await browserSupportsWebAuthnAutofill())) return;

  const resp = await fetch("/auth/webauthn/generate-authentication-options");
  const opts = await resp.json();

  try {
    const auth = await startAuthentication(opts, true);
    const data = await verifyAuthentication(auth);

    const location = new URL(data.redirect_url, window.location.href);
    window.location.replace(location);
  } catch (error) {
    alert("Could not perform Passkey sign in.");
  }
}

async function performRegistration() {
  const name = prompt("Please enter a name for this Passkey.");
  if (!name) return;

  const opts = await generateRegistrationOptions();
  let attestation = await startRegistration(opts);

  try {
    await verifyRegistration(name, attestation);
    alert("Passkey registered!");
  } catch {
    alert("Could not register Passkey, please try again later.");
  }
}

async function generateRegistrationOptions(): Promise<
  PublicKeyCredentialCreationOptionsJSON
> {
  const resp = await fetch("/auth/webauthn/generate-registration-options");
  return await resp.json();
}

async function verifyRegistration(
  name: string,
  response: RegistrationResponseJSON,
) {
  const resp = await fetch("/auth/webauthn/verify-registration", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "x-passkey-name": name,
    },
    body: JSON.stringify(response),
  });
  if (resp.status !== 204) {
    throw new Error(
      `Got unexpected status code verifying credential registration: ${resp.status}`,
    );
  }
}

interface LoginData {
  redirect_url: string;
}

async function verifyAuthentication(
  response: AuthenticationResponseJSON,
): Promise<LoginData> {
  const resp = await fetch("/auth/webauthn/verify-authentication", {
    method: "POST",
    headers: {
      "content-type": "application/json",
    },
    body: JSON.stringify(response),
  });
  if (resp.status !== 200) {
    throw new Error(
      `Got unexpected status code verifying authentication: ${resp.status}`,
    );
  }
  return await resp.json();
}
