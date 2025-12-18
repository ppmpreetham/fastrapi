import { useRef, useState } from "react";
import * as THREE from "three";
import { useFrame } from "@react-three/fiber";
import {
  PerspectiveCamera,
  OrbitControls,
  ScrollControls,
  useScroll,
  SpotLight,
} from "@react-three/drei";
import { Physics, RigidBody, CuboidCollider } from "@react-three/rapier";

import gsap from "gsap";
import { useGSAP } from "@gsap/react";

import { FastrapiModel } from "../models/fastrapimodel";
import { CrystalField } from "../models/MultiCrystal";
import RainDown from "./RainDown";
import { Rotate0, Rotate1, Rotate2, Rotate3, Rotate4 } from "./ContentRotate";
import Buildings from "../models/Buildings";
import { CloudsMulti } from "../models/Cloud";

const rotateComponents = [Rotate0, Rotate1, Rotate2, Rotate3, Rotate4];
const length = rotateComponents.length;
const PI = Math.PI;
const radius = 10;

function CameraScroller({ cameraRef, tl, setCount, count }: any) {
  const scroll = useScroll();
  useFrame(() => {
    const angle = scroll.offset * (length - 1) * PI;
    const newCount = Math.floor((angle - PI / 2) / PI) + 1;
    if (newCount !== count) setCount(newCount);
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
  const tl = useRef<gsap.core.Timeline>(null!);
  const cameraRef = useRef<THREE.PerspectiveCamera>(null!);
  const [count, setCount] = useState<number>(1);

  useGSAP(() => {
    tl.current = gsap.timeline();
  }, []);

  return (
    <>
      <OrbitControls enableZoom={false} enablePan={false} />
      <SpotLight position={[0, 5, 0]} angle={1 / 2} intensity={50} rotation={[1, 1, 1]} />

      <Physics gravity={[0, -9.81, 0]}>
        <ScrollControls pages={length} damping={0.25} infinite={false}>
          <PerspectiveCamera makeDefault position={[0, 0, radius]} ref={cameraRef} />
          <CameraScroller cameraRef={cameraRef} tl={tl} setCount={setCount} count={count} />
          <group>
            <RigidBody
              type="fixed"
              position={[0, -2, 0]}
              rotation={[-Math.PI / 2, 0, 0]}
              friction={2}
            >
              <mesh>
                <planeGeometry args={[100, 100]} />
                <meshStandardMaterial wireframe color="#222" />
              </mesh>
              <CuboidCollider args={[50, 50, 1]} position={[0, 0, -1]} />
            </RigidBody>
            <CloudsMulti count={40} planeScale={100} offset={[5, 5, 0]} />
            <CrystalField count={50} planeScale={40} offset={-2} />
            <RainDown planeScale={40} />
            <Buildings
              min={[1, 1, 1]}
              max={[10, 10, 10]}
              avoidRadius={12}
              count={100}
              planeScale={75}
            />
            <FastrapiModel />
            {rotateComponents.map((RotateComponent, index) =>
              count === index ? <RotateComponent key={index} place={index % 2 === 1} /> : null
            )}
          </group>
        </ScrollControls>
      </Physics>
    </>
  );
};

export default Experience;
