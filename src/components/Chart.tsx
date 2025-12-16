import { useRef } from "react";
import gsap from "gsap";
import { useGSAP } from "@gsap/react";

interface FrameworkData {
  name: string;
  value: number;
  color: string;
}

const data: FrameworkData[] = [
  { name: "*FastRAPI", value: 119887, color: "#c6ff00" },
  { name: "*Robyn", value: 74143, color: "#000000" },
  { name: "*FastAPI", value: 24440, color: "#000000" },
  { name: "Robyn", value: 19073, color: "#000000" },
  { name: "FastAPI", value: 4056, color: "#000000" },
];

export default function FrameworkChart() {
  const containerRef = useRef<HTMLDivElement>(null);
  const maxValue = Math.max(...data.map((d) => d.value));

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
        }
      );

      gsap.fromTo(
        ".value",
        { opacity: 0 },
        {
          opacity: 1,
          duration: 0.5,
          stagger: 0.1,
          delay: 1,
        }
      );
    }, containerRef);

    return () => ctx.revert();
  }, [maxValue]);

  return (
    <div className="min-h-fit fixed w-[33vw] font-random">
      <div ref={containerRef} className="mx-auto max-w-6xl space-y-4">
        {data.map((item) => (
          <div key={item.name} className="flex items-center gap-6">
            <div className="w-40 text-right text-3xl text-white">{item.name}</div>

            <div className="relative flex-1">
              <div className="bar h-14" style={{ backgroundColor: item.color }} />

              <span
                className={`value absolute -right-24 top-1/2 -translate-y-1/2 text-3xl font-bold [-webkit-text-stroke:0.25px_black] ${
                  item.color === "#c6ff00" ? "text-lime-300" : "text-white"
                }`}
              >
                {item.value.toLocaleString()}
              </span>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
