// import gsap from "gsap";
import { isMobile } from "../utils/helper"
import { RotHeaderText, RotSubText } from "./Texts"

interface featureItem {
  text: string
  offset: [number, number, number]
  mobileOffset: [number, number, number]
}

let upOffset = 5

const items: featureItem[] = [
  { text: "Middleware", offset: [-3, 2, 0], mobileOffset: [0, 2 + upOffset, 0] },
  { text: "Pydantic", offset: [3, 2, 0], mobileOffset: [0, 1 + upOffset, 0] },
  { text: "WebSockets", offset: [-3, -2, 0], mobileOffset: [0, 0 + upOffset, 0] },
  { text: "Swagger UI", offset: [-6, 0, 0], mobileOffset: [0, -1 + upOffset, 0] },
  { text: "Multithreaded", offset: [6, 0, 0], mobileOffset: [0, -2 + upOffset, 0] },
  { text: "Multi Core Scaling", offset: [3, -2, 0], mobileOffset: [0, -3 + upOffset, 0] },
]

const Features = ({ place }: { place: boolean }) => {
  return (
    <group scale={isMobile ? 0.4 : 1}>
      {items.map((item, index) => (
        <RotSubText
          key={index}
          place={place}
          text={item.text}
          size={isMobile ? 0.8 : 0.5}
          offset={isMobile ? item.mobileOffset : item.offset}
          color="white"
        />
      ))}
      <RotHeaderText
        place={place}
        text="Features"
        // size={isMobile ? 1.1 : 2}
      />
    </group>
  )
}

export default Features
