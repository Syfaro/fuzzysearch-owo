import * as Sentry from "@sentry/browser";
import "bulma-toast";

import "../sass/app.scss";

const appVersion = document.head.dataset.appVersion;

Sentry.onLoad(() => {
  const userID = <HTMLMetaElement | null>document.querySelector('meta[name="user-id"]');
  const initialScope = {
    user: { id: userID?.content || undefined },
  };

  Sentry.init({
    // @ts-ignore - set via esbuild define
    dsn: process.env.SENTRY_DSN,
    release: `fuzzysearch-owo-web@${appVersion}`,
    initialScope,
  });
});

import "../ts/pages";
