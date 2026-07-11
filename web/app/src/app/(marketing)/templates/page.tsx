import { MARKETING } from "../content"

export const metadata = { title: MARKETING.templates.title }

export default function Page() {
  return <div dangerouslySetInnerHTML={{ __html: MARKETING.templates.html }} />
}
