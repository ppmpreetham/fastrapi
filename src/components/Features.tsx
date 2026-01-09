// import gsap from "gsap";
import { RotHeaderText, RotSubText } from "./Texts"

const Features = ({ place }: { place: boolean }) => {
  return (
    <group>
      <RotSubText place={place} text="Middleware" size={0.5} offset={[-3, 2, 0]} color="white" />
      <RotSubText place={place} text="Pydantic" size={0.5} offset={[3, 2, 0]} color="white" />
      <RotSubText
        place={place}
        text="WebSockets (soon)"
        size={0.5}
        offset={[-3, -2, 0]}
        color="white"
      />
      <RotSubText place={place} text="Swagger UI" size={0.5} offset={[-6, 0, 0]} color="white" />
      <RotSubText place={place} text="Multithreaded" size={0.5} offset={[6, 0, 0]} color="white" />
      <RotSubText
        place={place}
        text="Multi Core Scaling"
        size={0.5}
        offset={[3, -2, 0]}
        color="white"
      />
      <RotHeaderText
        place={place}
        text="Features"
        // size={isMobile ? 1.1 : 2}
      />
      {/* feat: add many more here (images and other stuff) */}
    </group>
  )
}

export default Features
