import { redirect } from "next/navigation"

// The marketing site lives at camelmailer.com (separate deployment);
// this app is the dashboard only.
export default function Home() {
  redirect("/login")
}
