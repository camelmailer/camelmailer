import { MARKETING } from "./content"

export const metadata = { title: MARKETING.home.title }

export default function Page() {
  return <div dangerouslySetInnerHTML={{ __html: MARKETING.home.html }} />
}
