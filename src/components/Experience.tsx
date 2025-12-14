import { FastrapiModel } from "../models/fastrapimodel";
import { Environment, PerspectiveCamera, Text } from "@react-three/drei";
import { OrbitControls, ScrollControls, useScroll } from "@react-three/drei";
import gsap from "gsap";
import { useGSAP } from "@gsap/react";
import { CrystalField } from "../models/crystals/MultiChristal";
import * as THREE from "three";
import { useRef } from "react";
import { useFrame } from "@react-three/fiber";

const PI = Math.PI;

function CameraScroller({
  cameraRef,
  tl,
}: {
  cameraRef: React.RefObject<THREE.PerspectiveCamera>;
  tl: React.RefObject<gsap.core.Timeline>;
}) {
  const scroll = useScroll();
  const radius = 10;
  useFrame(() => {
    const angle = scroll.offset * 2 * PI;
    if (Math.abs((angle % PI) - PI / 2) < 0.1) {
      console.log("Snapped to odd multiple of PI/2");
    }
    const x = Math.sin(angle) * radius;
    const z = Math.cos(angle) * radius;
    if (cameraRef.current) {
      cameraRef.current.position.set(x, 0, z);
      cameraRef.current.lookAt(0, 0, 0);
    }
    tl.current.seek(scroll.offset * tl.current.duration());
  });

  return null;
}

const Experience = () => {
  const logoRef = useRef<THREE.Mesh>(null!);
  const groupRef = useRef<THREE.Mesh>(null!);
  const tl = useRef<gsap.core.Timeline>(null!);
  const cameraRef = useRef<THREE.PerspectiveCamera>(null!);

  useGSAP(() => {
    tl.current = gsap.timeline();
  }, []);

  return (
    <>
      <OrbitControls enableZoom={false} enablePan={false} />
      <Environment preset="dawn" />
      <ScrollControls pages={3} damping={0.25} infinite={false}>
        <PerspectiveCamera makeDefault position={[0, 0, 10]} ref={cameraRef} />
        <CameraScroller cameraRef={cameraRef} tl={tl} />
        <group ref={groupRef}>
          <CrystalField />
          <FastrapiModel ref={logoRef} />
          <Text color="#c9ff61" fontSize={2} font="/fonts/random.ttf" position={[0, 0, 0.01]}>
            FastRAPI
          </Text>
          <Text color="black" fontSize={2} font="/fonts/random.ttf" position={[0.05, -0.05, 0]}>
            FastRAPI
          </Text>
          <Text
            color="#c9ff61"
            fontSize={2}
            font="/fonts/random.ttf"
            position={[0, 0, -0.92]}
            rotation={[0, PI, 0]}
          >
            FastRAPI
          </Text>
          <Text color="black" fontSize={2} font="/fonts/random.ttf" position={[0.05, -0.05, 0]}>
            FastRAPI
          </Text>
        </group>
      </ScrollControls>
    </>
  );
};

export default Experience;
