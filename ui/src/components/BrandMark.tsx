import iconUrl from '../assets/icon.svg'

// The app icon (design/icon.svg, copied by scripts/gen-icons.sh) as a small
// rounded tile. Decorative: the name sits next to it.
export function BrandMark({ size = 20 }: { size?: number }) {
  return <img className="brand-mark" src={iconUrl} width={size} height={size} alt="" />
}
