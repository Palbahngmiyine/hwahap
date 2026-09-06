# 변경 기록

## 0.1.1

- 작업별 capability profile, task topology, reasoning depth와 실패 비용·복구 난도·영향 범위를 평가해 위임한다.
- 교체 가능한 모델 카탈로그를 run 시작 시 고정하고 실제 호스트의 모델·effort·도구·슬롯으로 배정을 검증한다.
- 작성 전 독립 검토자를 확보하고, 고위험 작업은 격리 환경의 실패·복구 검증 후 진행한다.
- 질문을 한 문장과 결과별 선택지로 정리하고 등록 후 상태 응답은 원문 참조로 축약한다.
- 이미 승인한 계획은 원문 요청·승인 참조·계획 digest를 보존해 실행한다.
- 작업자 연결 시간과 등록 후 수행 시간을 분리하고 중단·실패 후보를 백업해 같은 시도 이력에서 복구한다.
- 작업 단위의 인수 조건·테스트 연결, 구조 검토 지적의 처리, 의존 작업 재검증과 PR 검토 결속을 강화한다.

### 실행 기록 업그레이드

0.1.1은 `hwahap/v5`, 0.1.0은 `hwahap/v4` 실행 기록을 사용한다.
기존 v4 작업은 0.1.0 런타임으로 완료하거나 checkout·`.hwahap`·원본 바이너리를 함께 보존한다.
0.1.1은 별도 checkout과 새 run으로 시작한다. 필요한 승인 계획은 원문·참조를 통해 가져온다.
카탈로그 교체는 새 run부터 적용하며 진행 중 run은 기록된 모델·effort·역할을 유지한다.
모델이 제공 중단되면 해당 run은 복구를 기다린다.

[업그레이드 절차](https://github.com/Palbahngmiyine/hwahap/blob/v0.1.1/RELEASING.md#011-업그레이드) · [사용성 개선 근거](https://github.com/Palbahngmiyine/hwahap/blob/v0.1.1/docs/plans/usability-0.1.1.md)

## 0.1.0

독립 저장소, Codex plugin 패키지와 GitHub Actions의 플랫폼별 빌드·설치 검증·릴리스 배포를 제공한다.
