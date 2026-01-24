import { useRef } from "react"
import gsap from "gsap"
import { useGSAP } from "@gsap/react"
import clsx from "clsx"

interface FrameworkData {
  name: string
  value: number
  color: string
}

const data: FrameworkData[] = [
  { name: "*FastRAPI", value: 119887, color: "#c6ff00" },
  { name: "*Robyn", value: 74143, color: "#000000" },
  { name: "*FastAPI", value: 24440, color: "#000000" },
  { name: "Robyn", value: 19073, color: "#000000" },
  { name: "FastAPI", value: 4056, color: "#000000" },
]

export default function FrameworkChart({ isMobile }: { isMobile: boolean }) {
  const containerRef = useRef<HTMLDivElement>(null)
  const maxValue = Math.max(...data.map((d) => d.value))

  useGSAP(() => {
    const ctx = gsap.context(() => {
      gsap.fromTo(
        ".bar",
        { width: 0 },
        {
          width: (i: number) => `${(data[i].value / maxValue) * 100}%`,
          duration: 3,
          ease: "power3.out",
          stagger: 0.1,
        },
      )

      gsap.fromTo(
        ".value",
        { opacity: 0 },
        {
          opacity: 1,
          duration: 0.5,
          stagger: 0.1,
          delay: 1,
        },
      )
    }, containerRef)

    return () => ctx.revert()
  }, [maxValue, isMobile])

  return (
    <div
      ref={containerRef}
      className={clsx(
        "min-h-fit",
        isMobile ? "fixed h-fit w-[50vw]" : "fixed w-[clamp(400px,50vw,40rem)]",
        "font-random mx-auto",
        isMobile ? "" : "space-y-4",
        "pointer-events-none",
      )}
    >
      {data.map((item) => (
        <div key={item.name} className={clsx("flex items-center", isMobile ? "gap-4" : "gap-6")}>
          <div
            className={clsx(
              "text-start",
              isMobile ? "text-xl w-20" : "w-40 text-4xl",
              "text-white",
              isMobile ? "" : "[-webkit-text-stroke:0.1px_black]",
            )}
          >
            {item.name}
          </div>
          <div className={clsx("relative flex-1", isMobile && "flex items-center")}>
            <div
              className={clsx("bar origin-left", isMobile ? "h-4" : "h-14")}
              style={{ backgroundColor: item.color }}
            />

            {!isMobile && (
              <div
                className={clsx(
                  "value absolute -right-24 top-1/2 translate-x-6 -translate-y-1/2",
                  isMobile ? "" : "font-bold [-webkit-text-stroke:0.1px_black]",
                  {
                    "text-lime-300": item.color === "#c6ff00",
                    "text-white": item.color !== "#c6ff00",
                  },
                  isMobile ? "text-sm" : "text-4xl",
                )}
              >
                {item.value.toLocaleString()}
              </div>
            )}
          </div>
        </div>
      ))}
    </div>
  )
}
