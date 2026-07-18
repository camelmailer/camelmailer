# Cloud pricing and the public beta

CamelMailer runs in public beta on its EU cloud. Sending is free during the
beta, and paid plans launch soon. This page covers what the beta includes,
the plan that is coming, and where all of it lives in the dashboard.

Pricing applies to the **cloud** offering only. A self-hosted installation
keeps billing disabled and is never charged; see the note at the end.

## Public beta

While CamelMailer is in public beta, sending is free up to **5,000 emails
per calendar month**. There is nothing to pay and nothing to set up. Paid
plans launch after the beta, and you will be able to review them in the
dashboard before anything is charged.

## The Base package (coming soon)

The first paid plan is the **Base package**:

- **€5 per month for 5,000 emails**, billed monthly.
- The **cloud** offering, hosted on EU infrastructure.
- **Open and click tracking**, shown right in the app.

Pricing is launching soon, so the Base package appears in the dashboard as a
preview rather than a purchasable plan today.

## When you pass your quota

The plan preview also shows the two ways over-quota sending will work, so
you can decide which fits before plans go live:

- **Auto-upgrade.** Keep sending past your quota; the extra volume is billed
  automatically at the €5 / 5,000 rate.
- **Buy packages.** Add blocks of 5,000 emails at €5 each, so a month stays
  within a fixed budget.

This choice is a preview during the beta. You set it for real once paid
plans launch.

## In the dashboard

Under the organization's **Usage & Billing** page you get three sections:

- **Usage** shows the real sending volume over the last 30 days, summed
  across the organization's servers (and broken down per server when more
  than one has an API credential). Connect an API credential on a server to
  start measuring usage.
- **Plan** shows the public-beta banner, a quota meter against the 5,000
  per-month cap, the Base package preview, and the over-quota choice above.
- **Billing** appears only on the cloud offering when billing is enabled and
  you are an organization admin or owner: a card that hands off to the
  secure Stripe portal for subscription, payment methods and invoices.

## Self-hosted installations

Billing is the one deliberate cloud-only feature. A self-hosted install runs
with the `billing` config group disabled, so it is never charged, shows no
billing portal, and `POST …/billing/portal` returns `403 BillingDisabled`.
The Usage and Plan sections still render, so you can watch your own sending
volume. See [Configuration](configuration.md) and
[Accounts, RBAC and SSO](authentication.md) for the billing config group.
