# Frequently Asked Questions

## What does this site do?

FuzzySearch OwO allows you to add images by manual upload or continuous import
from sites like FurAffinity, and get notifications via email or Telegram when
those images are found on other sites like e621, F-list, and Reddit.

## How does it work?

Image similarity is determined with perceptual hashes. When you upload an image,
the content is hashed and searched against an existing database of images
downloaded from FurAffinity, Weasyl, e621, F-list, and more. It then keeps the
hash of the uploaded image to compare against every new image added to all of
those sites.

It's also [open source](https://github.com/Syfaro/fuzzysearch-owo).

## Where can I report an issue or suggest a feature?

Please send a message to [Syfaro](https://syfaro.net)! I would love to hear any
feedback you have on the site.

## What are the upcoming features?

You can find the public feedback and suggestion board
[here](https://owo-feedback.fuzzysearch.net). It is generally kept up to date
with requested and work in progress features.

For a list of what features have been added, see the [changelog](/changelog).

## How do I delete my account?

If you are no longer interested in using the service, you can easily delete your
account and all of your associated data on the [settings page](/user/settings).

## What data is collected?

A minimal amount of user data is collected. HTTP requests are routed through
Cloudflare and error messages are shared with Sentry. IP addresses are retained
for security purposes. Some aggregated and non-identifiable information is
recorded to understand how the site is used. User-provided information such as
names, emails, and images are stored securely and are only used for site
functionality.

## Why is an image not showing up?

The image database is not yet complete for FurAffinity and F-list. It is also
possible a submission was updated and the change was not detected yet.

For F-list, you may want to search [Lookout](https://lookout.best) as it seems
to have a more complete index at the moment.
