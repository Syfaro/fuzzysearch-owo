# Changelog

## v0.19.9 — April 28th, 2025

* Fixed an issue where common issues could cause performance issues.

## v0.19.8 — March 10th, 2025

* Fixed an issue with imports from a changed site layout.
* Internal updates.

## v0.19.7 — November 16th, 2024

* Internal updates.

## v0.19.6 — September 5th, 2024

* Added ability to create site alerts.
* Internal updates.

## v0.19.5 — July 23rd, 2024

* Internal updates.

## v0.19.4 — July 23rd, 2024

* Improved readability of notification emails.
* Internal updates.

## v0.19.3 — May 31st, 2024

* Fixed an issue where click to copy did not work as expected.

## v0.19.2 — May 31st, 2024

* Internal updates.

## v0.19.1 — May 30th, 2024

* Fixed an issue preventing account verification.
* Internal updates.

## v0.19.0 — March 21st, 2024

* Updated styling to support dark theme.
* Internal updates.

## v0.18.4 — February 29th, 2024

* Improved F-list timestamps for newly posted images.
* Internal updates.

## v0.18.3 — February 29th, 2024

* Added ability to use Passkeys for passwordless sign in.
* Fixed an issue where expired sessions could persist in the database.

## v0.18.2 — February 8th, 2024

* Internal updates.

## v0.18.1 — February 8th, 2024

* Fixed an issue where disabled accounts would not appear in check results.
* Fixed superfluous titles on certain dates.
* Internal refactoring of frontend code.

## v0.18.0 — February 7th, 2024

* Added ability to merge multiple images into one entry.
* Added beta feature opt-in ability.
* Improved media addition by automatically grouping identical files.
* Improved automatic imports to also search for previous uploads.
* Improved how dates are displayed.
* Improved UI with iconography.
* Fixed an issue that could cause Bluesky posts to stop processing.
* Internal updates.

## v0.17.2 — November 12th, 2023

* Improved Bluesky support with direct federated server access.
* Fixed an issue where background tasks could reduce site responsiveness.

## v0.17.1 – September 13th, 2023

* Fixed an issue causing Bluesky posts to not process as expected.

## v0.17.0 – September 13th, 2023

* Added Bluesky support.

## v0.16.0 — May 9th, 2023

* Added ability to allowlist entire sites.

## v0.15.3 — May 1st, 2023

* Fixed an issue causing Reddit data to not load.

## v0.15.2 — May 1st, 2023

* Internal updates.

## v0.15.1 — April 22nd, 2023

* Removed linking Twitter accounts due to API changes.

## v0.15.0 — February 25th, 2023

* Improved image performance and allowed PNGs for images with transparency.
* Improved URLs for accounts and media.
* Improved weak password feedback.
* Fixed an issue with the email unsubscribe link not working.
* Fixed an issue where deleting media would not remove it from the server.
* Fixed an issue where the wrong password screen would not render correctly.
* Internal updates.

## v0.14.4 — February 9th, 2023

* Internal updates.

## v0.14.3 — January 23rd, 2023

* Added API endpoint to lookup information about F-List image ID.
* Internal updates.

## v0.14.2 — October 10th, 2022

* Added password reset for users with linked email addresses.

## v0.14.1 — September 21st, 2022

* Added Twitter archive import to load all photos from account.
* Added more feedback when an action was performed.

## v0.14.0 — September 20th, 2022

* Added support for collecting images from Twitter.
* Added check tool to identify where you have already posted an image.

## v0.13.1 — September 2nd, 2022

* Added API for uploading images.
* Fixed the date associated with RSS feed items.

## v0.13.0 — September 2nd, 2022

* Added RSS feed generation for events.

## v0.12.1 — August 1st, 2022

* Fixed an issue related to fetching F-list content.

## v0.12.0 — July 28th, 2022

* Added FurAffinity scrap importing.
* Fixed an issue where DeviantArt submissions would only be added if no one else
  owned them.
* Fixed an issue that could cause F-list data to become stale.
* Internal tooling improvements, refactoring.

## v0.11.0 — June 21st, 2022

* Added daily or weekly digest feature to reduce frequency of emailed alerts.
* Fixed an issue where the FAQ link would only be visible to authenticated
  users.

## v0.10.2 — June 20th, 2022

* Added a frequently asked questions page.

## v0.10.1 — June 20th, 2022

* Fixed an issue causing F-list events to no longer display correctly.

## v0.10.0 — June 20th, 2022

* Added new event feed view which shows events for all media and provides
  options to filter results by site or allowlist status.

## v0.9.2 — June 20th, 2022

* Added changelog.
* Fixed an issue where music submissions could be imported.

## v0.9.1 — June 19th, 2022

* Fixed navigation bar to be usable on mobile.

## v0.9.0 — June 19th, 2022

* Added navigation bar to all pages.

## v0.8.3 — June 18th, 2022

* Added unsubscribe link to all notification emails.
* Fixed issue with email verification links being clicked by email providers.

## v0.8.2 — June 18th, 2022

* Fixed 404 error when email verification token was already used.

## v0.8.1 — June 18th, 2022

* Added selected account details when media view is filtered.

## v0.8.0 — June 18th, 2022

* Added feature to allowlist posters on sites.
* Added media view on account page and the ability to filter by account on
  sortable media page.
* Improved ability to change email address.
* Updated account verification options to include linking a Telegram account.

## v0.7.4 — April 15th, 2022

* Fixed an issue where certain types of files would generate incorrect
  notifications.
* Fixed an issue with emails when content contained emojis or other unicode
  symbols.

## v0.7.3 — March 31st, 2022

* Fixed an issue where users would be notified for their own submissions.
* Allowed multiple users to link the same account.

## v0.7.2 — March 28th, 2022

* Internal improvement to collect more details about errors.

## v0.7.1 — March 28th, 2022

* Fixed an issue with how media event metadata was calculated.

## v0.7.0 — March 28th, 2022

* Added new all media view with sorting options.
* Allowed uploading multiple images at once.
* Fixed an issue with account imports never being marked as completed.
* Internal improvement to collect client-side JavaScript errors.

## v0.6.1 — March 24th, 2022

* Updated homepage to include newly added Weasyl support.

## v0.6.0 — March 24th, 2022

* Added support for collecting submissions from Weasyl.
* Fixed issue with including own submissions in media events.
* Internal tooling improvements.

## v0.5.2 — March 23rd, 2022

* Internal tooling improvements.

## v0.5.1 — March 16th, 2022

* Fixed an issue where deleted subreddits would cause errors on submission
  collection.

## v0.5.0 — February 3rd, 2022

* Added ability to sign in with Telegram.
* Added notification settings.
* Improved user settings with display names and account deletion.
* Internal tooling improvements.

## v0.4.3 — February 1st, 2022

* Added site version number to footer.
* Fixed collection of IP address for user sessions.
* Updated websocket intervals and timeouts.

## v0.4.2 — February 1st, 2022

* Added site favicon.
* Disabled Patreon account linking due to API limitations.

## v0.4.1 — January 9th, 2022

* Fixed issues with pagination.

## v0.4.0 — January 9th, 2022

* Added a new view to see all uploaded media.
* Internal improvements to sending emails.

## v0.3.2 — January 9th, 2022

* Fixed an issue where requesting account verification would not update
  client-side state correctly.
* Internal improvements to account verification.

## v0.3.1 — January 8th, 2022

* Internal infrastructure improvements.

## v0.3.0 — January 8th, 2022

* Improved how user sessions are handled, including a view for users to manage
  their sessions.
* Fixed an issue with importing DeviantArt submissions.

## v0.2.2 — December 22nd, 2021

* Internal improvements to finding posts from Reddit.

## v0.2.1 — December 22nd, 2021

* Updated homepage to include newly added Reddit support.

## v0.2.0 — December 22nd, 2021

* Added support for monitoring Reddit.

## v0.1.0 — December 22nd, 2021

* Initial release.
