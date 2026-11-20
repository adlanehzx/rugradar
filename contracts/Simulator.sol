// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

/// Minimal interfaces of a Uniswap V2 / PancakeSwap V2 router and an ERC-20 token.
interface IRouterV2 {
    function getAmountsOut(uint256 amountIn, address[] calldata path) external view returns (uint256[] memory);

    function swapExactETHForTokensSupportingFeeOnTransferTokens(
        uint256 amountOutMin,
        address[] calldata path,
        address to,
        uint256 deadline
    ) external payable;

    function swapExactTokensForETHSupportingFeeOnTransferTokens(
        uint256 amountIn,
        uint256 amountOutMin,
        address[] calldata path,
        address to,
        uint256 deadline
    ) external;
}

interface IERC20Min {
    function balanceOf(address account) external view returns (uint256);
    function approve(address spender, uint256 amount) external returns (bool);
    function transfer(address to, uint256 amount) external returns (bool);
}

/// @title Simulator
/// @notice Never deployed. Its runtime bytecode is injected with an `eth_call` state override,
///         so a buy followed by a sell can be simulated against the live chain state for free,
///         without holding any funds.
/// @dev Status codes: 0 = ok, 1 = no route / no liquidity, 2 = buy reverted,
///      3 = approve reverted, 4 = sell reverted, 5 = nothing received on buy,
///      6 = wallet-to-wallet transfer reverted.
contract Simulator {
    struct Result {
        uint8 status;
        uint256 buyExpected;
        uint256 buyReceived;
        uint256 sellExpected;
        uint256 sellReceived;
        uint256 transferSent;
        uint256 transferReceived;
        uint256 buyGas;
        uint256 sellGas;
    }

    /// Ordinary, empty address used as the transfer recipient. The dead address is avoided on
    /// purpose: many tokens exclude it from fees, which would hide a transfer tax.
    address private constant PROBE = address(uint160(uint256(keccak256("rugradar.transfer-probe"))));

    receive() external payable {}

    function simulate(address router, address token, address wrappedNative)
        external
        payable
        returns (Result memory r)
    {
        IRouterV2 dex = IRouterV2(router);
        address[] memory buyPath = new address[](2);
        buyPath[0] = wrappedNative;
        buyPath[1] = token;

        // 1. Quote the buy.
        try dex.getAmountsOut(msg.value, buyPath) returns (uint256[] memory amounts) {
            r.buyExpected = amounts[1];
        } catch {
            r.status = 1;
            return r;
        }

        uint256 initialTokens = IERC20Min(token).balanceOf(address(this));

        // 2. Buy, measuring what actually arrives (fee-on-transfer aware).
        uint256 gasBefore = gasleft();
        try dex.swapExactETHForTokensSupportingFeeOnTransferTokens{value: msg.value}(
            0, buyPath, address(this), block.timestamp
        ) {
            r.buyGas = gasBefore - gasleft();
        } catch {
            r.status = 2;
            return r;
        }
        uint256 held = IERC20Min(token).balanceOf(address(this)) - initialTokens;
        r.buyReceived = held;
        if (held == 0) {
            r.status = 5;
            return r;
        }

        // 3. Wallet-to-wallet transfer of 1% to detect transfer taxes or transfer blocks.
        uint256 probeAmount = held / 100;
        if (probeAmount > 0) {
            uint256 probeBefore = IERC20Min(token).balanceOf(PROBE);
            try IERC20Min(token).transfer(PROBE, probeAmount) returns (bool success) {
                if (!success) { r.status = 6; return r; }
                r.transferSent = probeAmount;
                r.transferReceived = IERC20Min(token).balanceOf(PROBE) - probeBefore;
                if (r.transferReceived == 0) { r.status = 6; return r; }
            } catch {
                r.status = 6;
                return r;
            }
            held = IERC20Min(token).balanceOf(address(this)) - initialTokens;
        }

        // 4. Approve and sell everything that is left.
        try IERC20Min(token).approve(router, type(uint256).max) returns (bool success) {
            if (!success) { r.status = 3; return r; }
        }
        catch {
            r.status = 3;
            return r;
        }

        address[] memory sellPath = new address[](2);
        sellPath[0] = token;
        sellPath[1] = wrappedNative;
        try dex.getAmountsOut(held, sellPath) returns (uint256[] memory amounts) {
            r.sellExpected = amounts[1];
        } catch {
            r.status = 4;
            return r;
        }

        uint256 nativeBefore = address(this).balance;
        gasBefore = gasleft();
        try dex.swapExactTokensForETHSupportingFeeOnTransferTokens(
            held, 0, sellPath, address(this), block.timestamp
        ) {
            r.sellGas = gasBefore - gasleft();
            r.sellReceived = address(this).balance - nativeBefore;
            if (r.sellReceived == 0) { r.status = 4; return r; }
        } catch {
            r.status = 4;
            return r;
        }
    }
}
